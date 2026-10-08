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
    /// `target/` — the workspace's test compile, its tests, and the `--full`
    /// tiers that compile. (Until 2026-09-13 also the xtask verbs and every
    /// driven binary, and until 2026-10-05 the doctests; those have their own
    /// lanes below.)
    MainTarget,
    /// `target/` again, for the doctests alone (2026-10-05): the main lane's
    /// directory, as a lane of its own so that lane order no longer holds them
    /// behind the test run. They await the test compile instead
    /// ([`StageSpec::after_stages`]) and run beside the test run.
    ///
    /// Two lanes on one directory share its cargo lock, which is sound here
    /// because cargo holds it only while it compiles, and after the test
    /// compile neither row compiles anything: the doctests' graph is the test
    /// compile's units (`tests/profile_pin.rs`: 0 beyond them) and the test
    /// run's `--no-run` is a fingerprint check. Measured 2026-10-05: a `targo
    /// test --no-run` and a `--tests` run finished in 0.02 s and 0.43 s while
    /// `targo test --doc` sat in a 15 s doctest; rustdoc and the test binaries
    /// run outside the lock. (After a FAILED test compile both rows compile
    /// again, and the lock queues one behind the other, as it queues any two
    /// cargo children.)
    Doctests,
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
    /// the two `driver builds` stages that pre-compile them, and the rows whose
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
    /// `target-trust/` — the Trust verification lane's slots
    /// (`tools/trust-gate-all.sh --target-root`, one target dir per slot under
    /// it). Verified artifacts are built with different flags from every other
    /// lane's, so they share none of them.
    TrustTarget,
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
    /// The publication engine's own content guards — forbidden content,
    /// private references, gitleaks — over the export `pub stage` would build
    /// from this tree (`tools/export-content-scan.py`, `stages::export_content`).
    ExportContent,
    /// The offline suites over the scripts that build, sign, install and
    /// publish the app and its toolchain packages (`stages::DELIVERY_SUITES`):
    /// every one runs on stubs of its own making.
    DeliveryTooling,
    /// The atpkg end-to-end pack (`stages::ATPKG_DRIVEN_SUITE`), which drives
    /// the `atpkg` this stage builds first. A DRIVER-lane stage.
    AtpkgTooling,
    TrustContractProbe,
    /// The Trust verification lane, advisory (2026-09-27): the script's own
    /// verdict self-test, the ratchet's up-only check, then
    /// `tools/trust-gate-all.sh --per-commit` over the libraries and admitted
    /// forks the change touched, held to the ratchet floor
    /// (`stages::trust_advisory`). The whole tree is the release's: its facts,
    /// judged against the released commit's floors.
    TrustAdvisory,
    /// `--full` only: the startup-comparison harness's own test.
    StartCompare,
    LibcOracle,
    FreezeGate,
    /// The smokes' `aterm-gui`/`aterm-ctl`, pre-compiled at t0: the driver
    /// lane's first row, which the test run waits for.
    DriverBuilds,
    /// The binaries only the rows behind the first barrier drive — the live
    /// lanes' `aterm`, the redraw harness, the objc drivers — pre-compiled in
    /// the driver lane after every row the test run waits for, and waited for
    /// by nothing but that barrier ([`NOT_AWAITED`], 2026-09-29).
    TailDriverBuilds,
    /// The RELEASE `aterm` the paint and spin suites judge, built in its own
    /// lane at t0 so the measuring stage finds it warm. MEASURE tier.
    ConformanceRelease,
    /// The tests the parallel run skips because they MEASURE the machine or
    /// the release artifact (`stages::MEASURING_TESTS`, aterm-conformance's
    /// paint and spin), run with nothing else in flight. MEASURE tier.
    MeasuringTests,
    ControlSocketSmoke,
    GuiSmoke,
    RedrawConformance,
    /// The objc drivers (`stages::OBJC_DRIVES`), one row each, on macOS.
    ObjcDrives,
    /// The foreground handback lane (`stages::FOREGROUND_HANDBACK_SUITE`): a
    /// real shell's job control, in a private headless `aterm` this run built.
    ForegroundHandback,
    /// The render desync lane (`stages::RENDER_DESYNC_SUITE`): a net-zero
    /// alternate-screen flap an app cannot see, read by `status`, `resizes`
    /// and `cast drift` in a private headless `aterm` this run built.
    RenderDesync,
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

/// THE ROWS AN AFTER-LANES STAGE DOES NOT WAIT FOR, declared once
/// (2026-09-29): the one list [`crate::sched::awaited`] leaves out, so the test
/// run starts without them although it waits for their lane.
///
/// A row belongs here only when it is COMPILE-ONLY and everything it writes is
/// driven by rows behind the first barrier, which waits for it — so nothing
/// that awaits its lane reads a byte of it, and it starts no process for the
/// aterm-link harness to refuse. And it is its lane's LAST row before that
/// barrier, so lane order puts it behind every row the waiter does wait for,
/// never beside one. `sched`'s law test holds every entry to all three.
///
/// WHY (measured, 2026-09-29): the driver lane was one serial chain — the
/// objc drivers, the smokes, the live `aterm`, the redraw harness, then the
/// atpkg pack — and the test run waited for all of it, though only the pack
/// (and, under `--full`, the sealed rung and the smokes' `aterm-gui` it
/// drives) runs before the barrier. The objc drivers alone are a second
/// variant of the whole GUI graph (their dev-dependencies turn on features no
/// other driven binary has) and the redraw harness a third `aterm-gui`: on
/// 08c77ed37 the lane took 1183 s + 32 s beside a 1143 s test compile, so the
/// test run started 72 s after its compile had finished.
pub const NOT_AWAITED: [StageId; 1] = [StageId::TailDriverBuilds];

/// THE STAGES THAT PUT SOMETHING ON THE DESKTOP, declared once, each with the
/// name its `--no-windows` skip carries (2026-09-29). Measured that day on the
/// owner's screen while he worked: the objc drives held a window up for ~53 s,
/// a full-screen toggle among it (the toolbar drive orders its window front
/// and activates; the window drive shows and focuses its window, toggles full
/// screen and opens the About panel), and the redraw conformance put one up
/// for ~2 s; the GUI smoke's whole subject is a real window. The objc stage
/// skips whole: its quiet rows were not measured apart, and the owner's
/// screen is not where to measure them. Every other stage drives a
/// `--headless` instance, a stand-in, or no binary at all. Under `--no-windows`
/// ([`crate::Ctx::no_windows`]) each of these answers a named SKIP before it
/// builds or launches anything ([`crate::stages::run_stage`]), so a run on a
/// machine a person is using opens nothing — and, a skip being a skip, claims
/// no merge contract.
pub const WINDOWED: [(StageId, &str); 3] = [
    (StageId::GuiSmoke, "gui smoke"),
    (StageId::RedrawConformance, "redraw conformance"),
    (StageId::ObjcDrives, "objc drives"),
];

/// Why `--no-windows` skips a [`WINDOWED`] stage, as its skip row says it.
pub const NO_WINDOWS_REASON: &str = "--no-windows: opens a window on the desktop";

impl StageId {
    /// The named skip `--no-windows` answers for this stage: `Some` exactly
    /// for a [`WINDOWED`] stage.
    #[must_use]
    pub fn no_windows_skip(self) -> Option<String> {
        WINDOWED
            .iter()
            .find(|(id, _)| *id == self)
            .map(|(_, name)| format!("{name} ({NO_WINDOWS_REASON})"))
    }

    /// Whether a stage whose [`StageSpec::after_lanes`] names this stage's
    /// lane waits for it: every stage but the [`NOT_AWAITED`] ones.
    #[must_use]
    pub fn awaited(self) -> bool {
        !NOT_AWAITED.contains(&self)
    }

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
    /// Runs with nothing else in flight. Only the measuring tests, the two
    /// smokes and the Codex live upgrade, and only because they judge
    /// deadlines, frame rates and latencies: a stage whose verdict depends on
    /// how busy the machine is must own the machine.
    pub exclusive: bool,
    /// Lanes whose stages must all have FINISHED before this one starts —
    /// apart from stages behind an exclusive barrier declared after it, which
    /// cannot overlap it anyway, and the [`NOT_AWAITED`] compiles (see
    /// `crate::sched`). Only the test run uses it, to wait for the driver lane
    /// (see `plan`).
    pub after_lanes: Vec<Lane>,
    /// Stages that must have FINISHED before this one starts, under the same
    /// barrier rule as [`StageSpec::after_lanes`] (`crate::sched::awaited`).
    /// Empty except for the doctests, which await the test compile
    /// ([`Lane::Doctests`]); where [`MeasureOverlap::AfterLand`] holds the
    /// MEASURE tier's workspace build behind the land tier's on a host whose
    /// memory cannot hold them all at once; and where
    /// [`TrustOverlap::AfterTests`] holds the trust lane behind the land
    /// tier's builds and tests; and where [`LandBuilds::OneAtATime`] chains
    /// the land tier's builds.
    pub after_stages: Vec<StageId>,
}

/// THE ROWS THAT COMPILE AT WORKSPACE SCALE, declared once (2026-10-03): each
/// builds the `aterm-gui` graph (or, for the release artifact, the whole
/// `aterm` binary fat-LTO) and is what fills a small host's memory. Rows in one
/// lane run one at a time, so the builds a tier runs AT ONCE are the distinct
/// lanes among its rows here ([`concurrent_builds`]). The other compiling rows
/// — the xtask verbs, the oracle and L0 workspaces, the trust lane, whose
/// verifier is capped at RAM / 8 by `targo trust` itself — build far smaller
/// graphs and are not counted. Nor are the doctests here: they await the test
/// compile, so they never run beside it and the peak is unchanged; beside the
/// test run, where they take its place, [`builds_beside_tests`] counts them.
pub const WORKSPACE_BUILDS: [StageId; 5] = [
    StageId::TestCompile,
    StageId::Tippy,
    StageId::DriverBuilds,
    StageId::TailDriverBuilds,
    StageId::ConformanceRelease,
];

/// The memory one concurrent workspace build needs at its peak: 2 GiB.
///
/// MEASURED 2026-10-03 on an 8 GiB, 6-core Mac running `--full`: each
/// `aterm-gui` trustc process sat at 300-560 MB RSS, and a build has several
/// in flight at once, so one workspace build peaks at ~1.5-2 GiB.
pub const WORKSPACE_BUILD_BYTES: u64 = 2 * crate::disk::GIB;

/// What the host keeps for everything that is not a build — the OS, the
/// desktop, the gate itself and the tests the land tier runs beside its
/// compiles: 2 GiB.
///
/// WITH [`WORKSPACE_BUILD_BYTES`], MEASURED AGAINST THE SAME MACHINE: 8 GiB
/// less 2 holds 3 builds. The land tier alone runs exactly 3 (the test compile,
/// tippy, the driver lane) and passes there in ~50-60 min; `--full` added the
/// release build as a 4th, swap reached 6.9 of 8 GiB, every compile sat in
/// state U at 15-37% CPU, timing-bound stages failed and the run did not finish
/// in 2 hours. So both numbers are what that host holds and no more; a 12 GiB
/// host holds 5 and overlaps, the 128 GiB host 63.
pub const HOST_RESERVE_BYTES: u64 = 2 * crate::disk::GIB;

/// How many workspace builds `ram` bytes hold at once (at 2 GiB each beyond the
/// 2 GiB reserve).
#[must_use]
pub fn builds_held(ram: u64) -> u64 {
    ram.saturating_sub(HOST_RESERVE_BYTES) / WORKSPACE_BUILD_BYTES
}

/// The wired memory [`HOST_RESERVE_BYTES`] already stands for: 2.5 GiB.
/// Beyond it, every wired byte comes off the RAM the builds are sized from
/// ([`build_memory`]).
///
/// CHOSEN 2026-10-06, while the 8 GiB Mac the reserve was measured on held
/// 3.5 GiB wired with nothing building (228,350 16 KiB pages, `vm_stat`'s
/// figure and [`crate::exec::host_usable_bytes`]'s), so its healthy figure
/// could not be read here. A healthy 8 GiB Mac commonly wires 1.5-2.3 GiB, so
/// 2.5 GiB keeps every healthy host's plan and still takes a GiB off today's
/// degraded one, which three builds at once thrashed for 3 h 25 min
/// ([`land_builds`]). On 8 GiB any excess at all drops [`builds_held`] from 3
/// to 2: the measured plan had no slack. That first step therefore costs one
/// build of overlap ([`LandBuilds::TwoAtATime`]), never all of it; only a host
/// short of [`TWO_BUILDS_USABLE_BYTES`] builds one at a time. STILL TO DO: read
/// [`crate::exec::host_usable_bytes`] right after a reboot, idle and with the
/// owner's usual apps open, and set this above that figure (still 3.5 GiB
/// wired on 2026-10-06 at the review, so not measurable here yet).
pub const WIRED_ALLOWANCE_BYTES: u64 = 5 * crate::disk::GIB / 2;

/// The RAM every memory decision uses: the physical `ram`, less the memory
/// the host holds back from builds (`ram - usable`, wired on macOS) past
/// [`WIRED_ALLOWANCE_BYTES`]. Unread `usable` leaves `ram` alone; unread `ram`
/// stays unread.
#[must_use]
pub fn build_memory(ram: Option<u64>, usable: Option<u64>) -> Option<u64> {
    ram.map(|ram| usable.map_or(ram, |u| ram.min(u.saturating_add(WIRED_ALLOWANCE_BYTES))))
}

/// The usable memory two land-tier builds at once need: 5 GiB, their
/// [`WORKSPACE_BUILD_BYTES`] each plus half the [`HOST_RESERVE_BYTES`] for
/// what the wired figure leaves out (the desktop's pageable memory, the gate,
/// the tests beside them). Below it the land tier builds one at a time
/// ([`land_builds`]).
///
/// RULED 2026-10-06 (review): the measured degraded host — 4.5 GiB usable —
/// falls below it and builds one at a time, because no run there has shown
/// that two at once fit; a host wiring up to 3 GiB (5 GiB usable on 8 GiB,
/// e.g. a healthy Mac with GPU allocations from a Metal window) keeps two at
/// once and loses one build of overlap, not all of it.
pub const TWO_BUILDS_USABLE_BYTES: u64 = 2 * WORKSPACE_BUILD_BYTES + HOST_RESERVE_BYTES / 2;

/// The land tier's workspace builds, in the order they run when they run one
/// at a time ([`LandBuilds::OneAtATime`]): the test compile first, because
/// the test run waits for it; then the driver lane, which the test run also
/// waits for, its tail row included; then tippy, which nothing waits for.
pub const LAND_BUILD_ORDER: [StageId; 4] = [
    StageId::TestCompile,
    StageId::DriverBuilds,
    StageId::TailDriverBuilds,
    StageId::Tippy,
];

/// May the land tier's workspace builds run at once, on a host whose wired
/// memory may leave less than its RAM? A pure decision over (RAM, usable,
/// plan), tested without the real host ([`land_builds`]).
///
/// WHY (measured 2026-10-06, the 8 GiB Mac): with 3.5 GiB wired and nothing
/// building, the land tier's three builds at once ran 3 h 25 min without
/// finishing the test compile (`aterm-render` alone 3 h 12 min, every trustc
/// in state U, load 50-60), and the run was stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LandBuilds {
    /// Nothing changes: memory unread, no wired memory past the allowance,
    /// or enough left for every land-tier build at once.
    Together,
    /// Tippy waits for the test compile, so the land tier runs two builds at
    /// once (the test compile beside the driver lane, then tippy beside it):
    /// [`build_memory`] still holds two, and at least
    /// [`TWO_BUILDS_USABLE_BYTES`] is usable.
    TwoAtATime,
    /// The land tier's builds run one after another, in
    /// [`LAND_BUILD_ORDER`].
    OneAtATime,
}

/// Decide [`LandBuilds`] for `plan`. Nothing moves unless the wired memory
/// past [`WIRED_ALLOWANCE_BYTES`] leaves [`build_memory`] holding fewer builds
/// than the land tier runs at once AND fewer than the physical RAM holds — a
/// host already short on physical RAM keeps the plan it has always run. Then
/// two at once while [`build_memory`] holds two and
/// [`TWO_BUILDS_USABLE_BYTES`] is usable; otherwise one at a time.
#[must_use]
pub fn land_builds(ram: Option<u64>, usable: Option<u64>, plan: &[StageSpec]) -> LandBuilds {
    let (Some(ram), Some(build), Some(usable)) = (ram, build_memory(ram, usable), usable) else {
        return LandBuilds::Together;
    };
    let land = u64::try_from(concurrent_builds(plan, Tier::Land)).unwrap_or(u64::MAX);
    let held = builds_held(build);
    if held >= land.min(builds_held(ram)) {
        LandBuilds::Together
    } else if held >= 2 && usable >= TWO_BUILDS_USABLE_BYTES {
        LandBuilds::TwoAtATime
    } else {
        LandBuilds::OneAtATime
    }
}

impl LandBuilds {
    /// Chain the land tier's workspace builds: one at a time, each row of
    /// [`LAND_BUILD_ORDER`] the plan holds awaits every earlier one; two at a
    /// time, tippy awaits the test compile. Only scheduling moves: no row is
    /// added, removed or reordered. [`crate::sched::check_after_lanes`]
    /// accepts the chain: each wait reaches only rows declared before its
    /// waiter (the test compile, first in the ladder) or rows the waiter
    /// awaits as well.
    pub fn apply(self, plan: &mut [StageSpec]) {
        let present: Vec<StageId> = LAND_BUILD_ORDER
            .into_iter()
            .filter(|id| match self {
                LandBuilds::Together => false,
                LandBuilds::TwoAtATime => matches!(id, StageId::TestCompile | StageId::Tippy),
                LandBuilds::OneAtATime => true,
            })
            .filter(|id| {
                plan.iter()
                    .any(|s| s.id == *id && s.id.tier() == Tier::Land)
            })
            .collect();
        for (n, id) in present.iter().enumerate() {
            // The tail row follows the driver builds in their lane already.
            if *id == StageId::TailDriverBuilds {
                continue;
            }
            for s in plan.iter_mut().filter(|s| s.id == *id) {
                for earlier in &present[..n] {
                    if !s.after_stages.contains(earlier) {
                        s.after_stages.push(*earlier);
                    }
                }
            }
        }
    }
}

/// The first start line when wired memory takes RAM from the builds
/// ([`build_memory`] under `ram`): the usable figure, the wired one, and
/// what the plan does about it. `None` when nothing is held back.
fn usable_head(
    ram: u64,
    usable: Option<u64>,
    memtotal: bool,
    plan: &[StageSpec],
) -> Option<String> {
    let usable = usable.filter(|_| build_memory(Some(ram), usable) < Some(ram))?;
    // The wired figure is the difference of the two printed ones, so the
    // line adds up.
    let (ram_t, usable_t) = (tenths(ram), tenths(usable));
    let figures = format!(
        "verify: {} GiB of {} GiB{} is usable ({} GiB {})",
        tenths_text(usable_t),
        tenths_text(ram_t),
        memtotal_label(memtotal),
        tenths_text(ram_t.saturating_sub(usable_t)),
        if memtotal { "in use" } else { "wired" },
    );
    let remedy = if memtotal {
        "quitting other programs frees memory"
    } else {
        "a reboot frees wired memory"
    };
    Some(match land_builds(Some(ram), Some(usable), plan) {
        LandBuilds::OneAtATime => {
            format!("{figures} — the land tier builds one at a time; {remedy}")
        }
        LandBuilds::TwoAtATime => format!(
            "{figures} — the land tier builds two at a time, tippy after the test compile; \
             {remedy}"
        ),
        LandBuilds::Together => format!(
            "{figures} and holds {} workspace build(s) at once; {}",
            builds_held(build_memory(Some(ram), Some(usable)).unwrap_or(ram)),
            tiers_run(plan),
        ),
    })
}

/// The workspace builds `plan`'s `tier` rows run at once: the distinct lanes
/// among its [`WORKSPACE_BUILDS`] rows.
#[must_use]
pub fn concurrent_builds(plan: &[StageSpec], tier: Tier) -> usize {
    let mut lanes: Vec<Lane> = plan
        .iter()
        .filter(|s| s.id.tier() == tier && WORKSPACE_BUILDS.contains(&s.id))
        .map(|s| s.lane)
        .collect();
    lanes.sort_by_key(|l| *l as u8);
    lanes.dedup();
    lanes.len()
}

/// The workspace builds that can run beside `plan`'s test run: the land
/// tier's but the test compile, which ends before it (tippy and the tail
/// driver builds); the doctests, which since 2026-10-05 start when the test
/// compile ends and run beside it ([`Lane::Doctests`]) — rustdoc compiles each
/// crate's doctests, so they count as one build, the fail-closed figure; and
/// the MEASURE tier's release build, which [`measure_overlap`] holds only
/// behind the land tier's builds. Where tippy awaits the tail driver row
/// ([`LandBuilds::OneAtATime`]) the two never overlap, so one land build runs
/// beside the tests, not two.
#[must_use]
pub fn builds_beside_tests(plan: &[StageSpec]) -> usize {
    let doctests = usize::from(plan.iter().any(|s| s.id == StageId::Doctests));
    let chained =
        usize::from(plan.iter().any(|s| {
            s.id == StageId::Tippy && s.after_stages.contains(&StageId::TailDriverBuilds)
        }));
    concurrent_builds(plan, Tier::Land)
        .saturating_sub(1)
        .saturating_sub(chained)
        .max(chained)
        + doctests
        + concurrent_builds(plan, Tier::Measure)
}

/// The line a run prints at its start when the memory narrows the test pool
/// below `jobs` ([`crate::testrun::memory_jobs`]); `None` otherwise, or when
/// `plan` runs no tests. A cap: past [`crate::testrun::FLOOR_JOBS`] the pool
/// starts more only while the load allows ([`crate::testrun::Limits::by_load`]).
#[must_use]
pub fn test_pool_note(ram: Option<u64>, plan: &[StageSpec], jobs: usize) -> Option<String> {
    let pool = crate::testrun::memory_jobs(ram, plan);
    if pool >= jobs || !plan.iter().any(|s| s.id == StageId::Test) {
        return None;
    }
    Some(match ram {
        None => format!("verify: the test pool runs at most {pool} binaries at once"),
        Some(_) => format!(
            "verify: the test pool runs at most {pool} binaries at once, beside {} workspace build(s)",
            builds_beside_tests(plan),
        ),
    })
}

/// The start lines about this host's memory: what it holds and what the plan
/// runs at once, stated once, then each change it forces
/// ([`measure_overlap`], [`trust_overlap`], [`test_pool_note`]). Empty when
/// nothing changes. Where wired memory takes RAM from the builds
/// ([`build_memory`]) the head states the usable figure instead, and is the
/// one line that says the land tier builds one at a time ([`land_builds`]);
/// every decision then uses that smaller RAM. `memtotal`: the RAM figure is
/// Linux's `MemTotal` ([`crate::Ctx::host_memtotal`]), so every GiB figure is
/// in the OS's units, not the installed size.
#[must_use]
pub fn memory_notes(
    ram: Option<u64>,
    usable: Option<u64>,
    memtotal: bool,
    plan: &[StageSpec],
    jobs: usize,
) -> Vec<String> {
    let build = build_memory(ram, usable);
    let changes: Vec<String> = [
        measure_overlap(build, plan).note(memtotal),
        trust_overlap(build, plan).note(memtotal),
        test_pool_note(build, plan, jobs),
    ]
    .into_iter()
    .flatten()
    .collect();
    let serial = land_builds(ram, usable, plan) != LandBuilds::Together;
    if changes.is_empty() && !serial {
        return changes;
    }
    let head = match ram {
        None => "verify: this host's RAM could not be read".to_string(),
        Some(ram) => usable_head(ram, usable, memtotal, plan).unwrap_or_else(|| {
            format!(
                "verify: {} GiB RAM{} holds {} workspace build(s) at once; {}",
                gib_tenths(ram),
                memtotal_label(memtotal),
                builds_held(ram),
                tiers_run(plan),
            )
        }),
    };
    std::iter::once(head).chain(changes).collect()
}

/// "the land tier runs N[ and the MEASURE tier M]": the workspace builds each
/// tier of `plan` runs at once.
fn tiers_run(plan: &[StageSpec]) -> String {
    let measure = concurrent_builds(plan, Tier::Measure);
    format!(
        "the land tier runs {}{}",
        concurrent_builds(plan, Tier::Land),
        if measure > 0 {
            format!(" and the MEASURE tier {measure}")
        } else {
            String::new()
        },
    )
}

/// The unit label a GiB figure carries when it is Linux's `MemTotal`.
fn memtotal_label(memtotal: bool) -> &'static str {
    if memtotal { " (MemTotal)" } else { "" }
}

/// The RAM, in whole GiB, that holds `builds` workspace builds at once.
fn ram_for(builds: usize) -> u64 {
    let builds = u64::try_from(builds).unwrap_or(u64::MAX);
    HOST_RESERVE_BYTES.saturating_add(builds.saturating_mul(WORKSPACE_BUILD_BYTES))
        / crate::disk::GIB
}

/// May the MEASURE tier's workspace builds overlap the land tier's, on a host
/// with this much memory? A pure decision over (RAM, plan), so it is tested
/// without the real host ([`measure_overlap`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MeasureOverlap {
    /// Nothing changes: one tier alone, or memory enough for every build at
    /// once.
    Overlaps,
    /// The MEASURE tier's builds wait for every land-tier build to finish.
    AfterLand {
        /// The host's physical memory, in bytes.
        ram: u64,
        /// The workspace builds it holds at once ([`builds_held`]).
        holds: u64,
        /// The workspace builds the land tier runs at once.
        land: usize,
        /// The workspace builds the MEASURE tier runs at once.
        measure: usize,
    },
    /// The MEASURE tier's builds wait as in [`MeasureOverlap::AfterLand`]
    /// because the host's memory could not be read: the serial order is the
    /// one that cannot swap a small host, so an unknown host gets it.
    RamUnread,
}

/// Decide [`MeasureOverlap`] for `plan` on a host with `ram` bytes: the
/// MEASURE tier's builds wait when the two tiers' concurrent workspace builds
/// together exceed what the memory holds, or when the memory is unknown.
#[must_use]
pub fn measure_overlap(ram: Option<u64>, plan: &[StageSpec]) -> MeasureOverlap {
    let land = concurrent_builds(plan, Tier::Land);
    let measure = concurrent_builds(plan, Tier::Measure);
    match ram {
        Some(ram) if land > 0 && measure > 0 => {
            let holds = builds_held(ram);
            if u64::try_from(land + measure).unwrap_or(u64::MAX) > holds {
                MeasureOverlap::AfterLand {
                    ram,
                    holds,
                    land,
                    measure,
                }
            } else {
                MeasureOverlap::Overlaps
            }
        }
        None if land > 0 && measure > 0 => MeasureOverlap::RamUnread,
        _ => MeasureOverlap::Overlaps,
    }
}

impl MeasureOverlap {
    /// Hold every MEASURE-tier workspace build behind every land-tier one.
    /// Only scheduling moves: no row is added, removed or reordered.
    pub fn apply(self, plan: &mut [StageSpec]) {
        if self == MeasureOverlap::Overlaps {
            return;
        }
        let land: Vec<StageId> = plan
            .iter()
            .filter(|s| s.id.tier() == Tier::Land && WORKSPACE_BUILDS.contains(&s.id))
            .map(|s| s.id)
            .collect();
        for s in plan.iter_mut() {
            if s.id.tier() == Tier::Measure && WORKSPACE_BUILDS.contains(&s.id) {
                s.after_stages.clone_from(&land);
            }
        }
    }

    /// The line a run prints at its start when the builds wait; `None` when
    /// nothing changes. [`memory_notes`] states the RAM before it; `memtotal`
    /// labels the needed figure as `MemTotal`, as [`memory_notes`] does.
    #[must_use]
    pub fn note(self, memtotal: bool) -> Option<String> {
        match self {
            MeasureOverlap::Overlaps => None,
            MeasureOverlap::RamUnread => {
                Some("verify: the MEASURE tier builds after the land tier's".to_string())
            }
            MeasureOverlap::AfterLand { land, measure, .. } => Some(format!(
                "verify: the MEASURE tier builds after the land tier's ({} GiB{} runs them together)",
                ram_for(land + measure),
                memtotal_label(memtotal),
            )),
        }
    }
}

/// `ram` bytes in GiB, FLOORED to a tenth, never rounded up: Linux's MemTotal
/// sits a few percent under the installed RAM, and "8 GiB holds 2" would not
/// add up where the 7.7 GiB the decision used does.
fn gib_tenths(ram: u64) -> String {
    tenths_text(tenths(ram))
}

/// `bytes` in tenths of a GiB, floored ([`gib_tenths`]).
fn tenths(bytes: u64) -> u64 {
    bytes.saturating_mul(10) / crate::disk::GIB
}

/// A count of tenths of a GiB as text: "4.5", or "8" for a whole figure.
fn tenths_text(tenths: u64) -> String {
    if tenths.is_multiple_of(10) {
        format!("{}", tenths / 10)
    } else {
        format!("{}.{}", tenths / 10, tenths % 10)
    }
}

/// The row whose end is the land tier's test run's end, for the trust lane
/// to await ([`trust_overlap`]): the test run itself, since 2026-10-05.
///
/// Until then it was the doctests, the main lane's row after the test run,
/// because the test run waits on the driver lane and
/// [`crate::sched::check_after_lanes`] refused awaiting a stage that waits on
/// lanes. Once the doctests ran beside the test run their end no longer meant
/// its end, so the check now accepts such a stage when its waiter awaits every
/// row it awaits too ([`TrustOverlap::apply`] adds them: they end before the
/// test run starts, so they move nothing).
const TEST_RUN_END: StageId = StageId::Test;

/// May the trust lane verify beside the workspace builds and the land tier's
/// test run, on a host with this much memory? A pure decision over (RAM,
/// plan), the same shape as [`MeasureOverlap`] ([`trust_overlap`]).
///
/// WHY (measured 2026-10-04, an 8 GiB, 6-core Mac, two merge-contract runs of
/// one tree): verifying while the test stage compiled and ran (load 54), the
/// verifier's per-function budgets tripped on load and proved counts swung
/// run to run — aterm-lexicon 90 then 40, aterm-policy 62 then 42,
/// aterm-wasm 18 then 127 — so its ratchet rows failed or passed by chance.
///
/// COST: on that host the lane's wall now follows the ~50-60 min land tier
/// instead of overlapping it, and the exclusive tail follows the lane, so a
/// change touching a verified library can take a merge-contract run past
/// 2 h (not yet measured).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrustOverlap {
    /// Nothing changes: no trust lane or no land-tier build in the plan, or
    /// memory to spare beyond the workspace builds that run at once.
    Overlaps,
    /// The trust lane waits for every workspace build (the MEASURE tier's
    /// too, in `--full`), the land tier's test run and its doctests.
    AfterTests {
        /// The host's physical memory, in bytes.
        ram: u64,
        /// The workspace builds it holds at once ([`builds_held`]).
        holds: u64,
        /// The workspace builds the land tier runs at once.
        land: usize,
        /// The MEASURE tier's builds that run beside the land tier's — 0
        /// when there are none, or when [`measure_overlap`] already holds
        /// them behind the land tier's.
        measure: usize,
        /// The RAM, in whole GiB, from which the lane runs beside every
        /// workspace build of both tiers.
        need: u64,
    },
    /// The trust lane waits as in [`TrustOverlap::AfterTests`] because the
    /// host's memory could not be read.
    RamUnread,
}

/// Decide [`TrustOverlap`] for `plan` on a host with `ram` bytes: the trust
/// lane waits when the memory holds no more than the workspace builds that
/// run at once — the land tier's, plus the MEASURE tier's where
/// [`measure_overlap`] lets them overlap — or when the memory is unknown.
#[must_use]
pub fn trust_overlap(ram: Option<u64>, plan: &[StageSpec]) -> TrustOverlap {
    let land = concurrent_builds(plan, Tier::Land);
    if land == 0 || !plan.iter().any(|s| s.id == StageId::TrustAdvisory) {
        return TrustOverlap::Overlaps;
    }
    match ram {
        Some(ram) => {
            let measure = if measure_overlap(Some(ram), plan) == MeasureOverlap::Overlaps {
                concurrent_builds(plan, Tier::Measure)
            } else {
                0
            };
            let holds = builds_held(ram);
            if holds <= u64::try_from(land + measure).unwrap_or(u64::MAX) {
                TrustOverlap::AfterTests {
                    ram,
                    holds,
                    land,
                    measure,
                    need: ram_for(land + concurrent_builds(plan, Tier::Measure) + 1),
                }
            } else {
                TrustOverlap::Overlaps
            }
        }
        None => TrustOverlap::RamUnread,
    }
}

impl TrustOverlap {
    /// Hold the trust lane behind every workspace build of either tier,
    /// `TEST_RUN_END` (the test run) and the doctests beside it. Only
    /// scheduling moves: no row is added, removed or reordered. Two of those
    /// wait themselves, and the trust lane awaits what they await too, which
    /// [`crate::sched::check_after_lanes`] requires: the MEASURE tier's
    /// release build may wait behind the land tier's builds
    /// ([`measure_overlap`]), and the test run waits on the driver lane's
    /// awaited rows, which end before it starts.
    pub fn apply(self, plan: &mut [StageSpec]) {
        if self == TrustOverlap::Overlaps {
            return;
        }
        let run = plan.iter().position(|s| s.id == TEST_RUN_END);
        let waits: Vec<StageId> = plan
            .iter()
            .enumerate()
            .filter(|&(j, s)| {
                WORKSPACE_BUILDS.contains(&s.id)
                    || s.id == TEST_RUN_END
                    || s.id == StageId::Doctests
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

    /// The line a run prints at its start when the trust lane waits; `None`
    /// when nothing changes. [`memory_notes`] states the RAM before it;
    /// `memtotal` labels the needed figure as [`MeasureOverlap::note`] does.
    #[must_use]
    pub fn note(self, memtotal: bool) -> Option<String> {
        const WAITS: &str = "verify: the trust lane verifies after the tests and builds";
        match self {
            TrustOverlap::Overlaps => None,
            TrustOverlap::RamUnread => Some(WAITS.to_string()),
            TrustOverlap::AfterTests { need, .. } => Some(format!(
                "{WAITS} ({need} GiB{} runs it beside them)",
                memtotal_label(memtotal)
            )),
        }
    }
}

/// The target directory a lane's cargo children use. `None` for [`Lane::Pure`],
/// which has none.
///
/// `MainTarget` is `<root>/target`, cargo's own default: its children are
/// handed no directory and inherit none — a caller's `CARGO_TARGET_DIR` never
/// reaches a child ([`crate::CHILD_ENV_REMOVED`]). `Doctests` is the same
/// directory, handed the same way. The side lanes are absolute
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
        Lane::MainTarget | Lane::Doctests => Some(ctx.root.join("target")),
        // Spelled as `stages::tippy_cmd` spells it, which is unchanged.
        Lane::TippyTarget => Some(ctx.root.join("target-tippy")),
        Lane::FreezeGateTarget => under_root("tools/freeze-safety-gate/target"),
        Lane::LibcOracleTarget => under_root("libc-oracle/target"),
        Lane::XtaskTarget => under_root("target-xtask"),
        Lane::DriverTarget => under_root("target-drivers"),
        // The release `aterm` the measuring stage hands paint and spin in
        // `ATERM_PAINT_BIN` / `ATERM_SPIN_BIN` (`release_bin` in
        // `crates/aterm-conformance/tests/support/mod.rs` builds none).
        Lane::ConformanceRelease => under_root("target/conformance-release"),
        Lane::TrustTarget => under_root("target-trust"),
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

/// Build the run's stage list: every stage [`declared`] for its scope whose
/// tier its mode runs ([`crate::cli::Mode::runs`]).
///
/// Two things can remove a stage entirely (as opposed to skipping it):
///  * a narrowing that does not compile a stage's crate drops it, because
///    there is nothing for it to run: `aterm-link` for the sealed fabric
///    lane, `aterm-conformance` for the measuring tests and their release
///    prime ([`measures_paint_and_spin`]);
///  * the MODE picks the tiers ([`Tier`]): the default runs the LAND tier and
///    not the MEASURE tier — and its verdict NAMES every MEASURE stage it left
///    out ([`tier_titles`]), so the removal is never silent — `--measure` runs
///    the MEASURE tier alone, and `--full` both plus its own ([`FULL_ONLY`]).
///
/// Three things move WHEN a stage runs, never whether or where it prints, each
/// decided from the RAM the builds can have ([`Ctx::build_memory`]): on a
/// host whose wired memory leaves too little for the land tier's builds at
/// once, their chain one behind another ([`land_builds`]); on one whose
/// memory cannot hold both tiers' workspace builds at once, the MEASURE
/// tier's wait for the land tier's ([`measure_overlap`]); and on one with no
/// room beyond the land tier's builds, the trust lane's wait for them and the
/// tests ([`trust_overlap`]).
///
/// Nothing else is conditional here. Absent TOOLS produce skips inside a stage,
/// never a missing stage, because a stage that vanishes is a stage nobody
/// notices was not run.
#[must_use]
pub fn plan(ctx: &Ctx) -> Vec<StageSpec> {
    let mut v: Vec<StageSpec> = declared(&ctx.scope)
        .into_iter()
        .filter(|s| ctx.mode.runs(s.id.tier()))
        .collect();
    land_builds(ctx.host_memory, ctx.host_usable, &v).apply(&mut v);
    let ram = ctx.build_memory();
    measure_overlap(ram, &v).apply(&mut v);
    trust_overlap(ram, &v).apply(&mut v);
    v
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
        // in an exclusive stage of their own (the MEASURE tier's
        // `StageId::MeasuringTests`), so the run no longer holds its start for
        // a compile. NOR, since 2026-09-29, for the driver lane's
        // last row, the tail driver builds: a compile of what only the rows
        // behind the first barrier drive ([`NOT_AWAITED`]).
        StageSpec {
            after_lanes: vec![Lane::DriverTarget],
            ..spec(StageId::Test, format!("test ({label})"), Lane::MainTarget)
        },
        // THE DOCTESTS, BESIDE THE TEST RUN (2026-10-05). They need the test
        // compile and nothing else, so they await it ([`Lane::Doctests`] says
        // why sharing `target/` is sound) instead of queueing behind the test
        // run in the main lane. MEASURED on 2a73ffa2d (`--no-windows`, cold
        // compile): the test compile ended at +466.8 s, the test run ran to
        // +723.7 s, then the doctests took 37.1 s and the first barrier
        // started at +760.8 s — 37 s of a 795 s run on the critical path, for
        // 84 crates' doctests run one crate at a time (~0.9 s per crate with
        // doctests, merged compile included). Beside a 257 s test run they end
        // long before it does. Still printed here, after the test row.
        StageSpec {
            after_stages: vec![StageId::TestCompile],
            ..spec(
                StageId::Doctests,
                format!("doctests ({label})"),
                Lane::Doctests,
            )
        },
    ];
    v.push(spec(
        StageId::Tippy,
        format!("tippy lint ({label})"),
        Lane::TippyTarget,
    ));
    // FORMATTING, right after the lint it belongs beside. It is `xtask gate
    // lint --fmt-only`: the formatter lane's three passes — `targo-fmt --all`
    // over the workspace, the per-file sweep over every tracked source, and
    // `fmt editions` — and no other lane. `XtaskTarget` because it runs
    // through the xtask binary; the check itself needs no compiler and cost
    // 7.5 s over 1,761 files on two measured runs.
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
    // EXPORT CONTENT (2026-09-29) — the publication engine's content guards
    // over the export of this tree, run by the engine's own code
    // (`tools/export-content-scan.py`). Export blockers stopped five releases in
    // a row (0.94, 0.95, 0.98, 0.99 twice), each found only by the release
    // preflight's dry run, because the commits that carried them landed through
    // a contract that never ran the scan. `Lane::Pure`, at t0: it greps an
    // export in a scratch directory and compiles nothing, so it overlaps the
    // test compile the way the grep guards do. Measured on 18 cores: 19-22 s
    // at load ~11 and 43 s at load ~110, ~165 CPU-seconds either way — most of
    // them the engine's own one grep per pattern, per pass, over the ~150 MB
    // export, with gitleaks running beside it.
    v.push(spec(
        StageId::ExportContent,
        "export content (the publication engine's forbidden-content, private-reference and gitleaks guards over this tree's export)",
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
        "delivery tooling (installer channel and guards, cargo pin, export policy and content scan, release preflight, shape and after-cut, site sync, dev signing identity, fabric demo cleanup, atpkg index and publish producers, publisher bootstrap; offline)",
        Lane::Pure,
    ));
    v.push(spec(
        StageId::TrustContractProbe,
        "trust contract probe (self-field ensures proves; off-switch no ICE)",
        Lane::Pure,
    ));
    // THE TRUST VERIFICATION LANE (2026-09-27), advisory, beside the probe it
    // belongs with — a LAND-tier stage, so the merge contract runs it. Its own
    // lane: it compiles in its own target dirs and verifies, so on a host with
    // memory to spare it starts at t0 and overlaps the test compile and run; on
    // one whose memory holds no more than the workspace builds that run at
    // once it waits for them (both tiers', in `--full`) and the land tier's
    // test run and doctests ([`trust_overlap`], 2026-10-04: under that load
    // its budgets tripped and proved counts swung run to run). The exclusive tail
    // waits for it the way it waits for every stage declared before it. In every
    // mode it verifies the libraries and admitted forks a change touched, the
    // heaviest named and left to the release's whole-tree run
    // (`stages::trust_advisory` says why, with the measurement); the row's own
    // verdict line names what it verified and what it did not.
    v.push(spec(
        StageId::TrustAdvisory,
        "trust advisory lane (targo trust check --lib --allow-l0-gaps: the libraries a change touched, the heaviest left to the release's whole-tree run; the up-only ratchet floor)",
        Lane::TrustTarget,
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
    // DRIVER BUILDS (2026-09-13): the binaries the driver lane's rows drive,
    // compiled at t0 in their own lane, in two rows since 2026-09-29 — this
    // one, the smokes' `aterm-gui`/`aterm-ctl` (which the `--full` sealed rung
    // drives before the barrier too), and the tail driver builds below, the
    // lane's last row before the barrier, for what only the rows behind it
    // drive. Every driver stage below still runs its own build argv — a
    // fingerprint no-op after these — so nothing is proven from their
    // binaries that its own stage did not ask cargo for. What they remove is
    // those compiles running one after another inside the exclusive tail.
    // Declared before the smokes, because a stage after an exclusive barrier
    // could not start until the barrier finished.
    v.push(spec(
        StageId::DriverBuilds,
        "driver builds (the smokes' aterm-gui and aterm-ctl)",
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
    // THE TAIL DRIVER BUILDS (2026-09-29): the live lanes' `aterm`, the redraw
    // harness and the objc drivers — the binaries only the rows behind the
    // first barrier drive — split out of the driver builds row above and
    // declared LAST before that barrier. Lane order runs it after every row
    // the test run waits for, the test run does not wait for it
    // ([`NOT_AWAITED`]: a compile-only row whose every product is driven
    // behind the barrier), and the barrier does, so no row drives one of its
    // binaries before it has finished. The same target dir as the rest of the
    // lane, so it reuses every unit the smokes' build compiled: splitting it
    // into a lane of its own would compile those twice (`aterm-spec` alone
    // took 1215 s of the objc drivers' build under load, measured).
    v.push(spec(
        StageId::TailDriverBuilds,
        "tail driver builds (the live lanes' aterm, the redraw harness and the objc drivers)",
        Lane::DriverTarget,
    ));
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
    // tier's barrier in every scope — the deadline tests that once stood before
    // it left the ladder on 2026-09-30 (their deadlines became hang detectors,
    // so they run in the parallel test run) — and the driver-lane rows
    // declared after it run with nothing else in flight.
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
    // THE OBJC DRIVES, one stage over `stages::OBJC_DRIVES`, unconditional for
    // the redraw gate's reason. libtest cannot host AppKit, so each driver is
    // an `[[example]]` owning `fn main` and this is what invokes it:
    // - `objc_live_class_audit`: the registered WinitWindowDelegate and
    //   WinitView against the runtime's own authority;
    // - `objc_ime_drive`: a composition through WinitView's NSTextInputClient rows;
    // - `objc_toolbar_drive`: the real tab strip, 27 drawn states, four classes;
    // - `objc_window_drive`: the real window's whole surface;
    // - `objc_event_drive`: a real NSEvent of every type through every
    //   event-taking row and `sendEvent:` (the v0.72.0 crash class).
    // (The MainThreadBound container drive left this stage on 2026-09-28: it is
    // `aterm-objc`'s `harness = false` test `main_thread_bound_drive`, which
    // the test run runs.)
    v.push(spec(
        StageId::ObjcDrives,
        "objc drives (live-class audit, IME, toolbar, window and event, on a real AppKit)",
        Lane::DriverTarget,
    ));
    // THE FOREGROUND HANDBACK (2026-09-26), a row of the driver lane behind the
    // smokes (the render desync lane, below, was added after it on 2026-09-28).
    // `tools/test-foreground-handback.sh` drives a private headless `aterm` —
    // THE one binary, which no other stage builds — and until this row nothing
    // ran it: by hand it found no `<root>/target/debug/aterm` and answered with
    // a code that is not a failure (2). The stage builds `-p aterm --bin aterm`
    // in this lane (the tail driver builds row has compiled it) and hands the
    // lane that binary. HERE, behind the smokes' barrier, because every stage
    // after the barrier in a `--fast` run is in this lane, so nothing else is
    // in flight while it waits on real shells under deadlines (`ctl await …
    // timeout=`), and a compile beside it turns a slow machine into a red row.
    // (In a `--full` run the tiers below run after the same barrier and do
    // overlap it, as they overlap the redraw gate and the
    // drives; the lane's deadlines are 10-20 s against a ~7 s run.) Declared
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
    // THE RENDER DESYNC LANE (2026-09-28), the handback lane's sibling in
    // shape and in place: `tools/test-render-desync.sh` drives a private
    // headless `aterm` built this run (the same build child, a fingerprint
    // no-op by now), so it sits in this lane behind the smokes' barrier and is
    // never removed by a narrowing. It reproduces the incident a live tab hit —
    // a 64 -> 63 -> 64 flap on the alternate screen shifts the screen up one
    // row, and a Claude Code that read an unchanged size drew diffs onto it
    // for good — with a stand-in app whose SIGWINCH is blocked across the two
    // resizes, so the MASK orders the race, not a sleep; and it proves the
    // three surfaces that exist to see it (`status` render=, `resizes`,
    // `cast drift`) name it, name a healed flap as healed, and name a flap
    // with nothing between its halves as undone by the engine, and name a flap
    // whose halves 1.6 s split as one return (`cast drift`); it drives the
    // repaint nudge end to end (the `nudge`, `log` and `norepaint` variants:
    // a displaced stand-in is repaired in a closed loop, the log stage touches
    // no PTY, and a program that repaints without a full clear is tried once
    // and given up on), each waiting out the nudge's own person and quiet
    // gates. ~16 s (~48 s until 2026-10-05, when its variants began running three
    // private instances at a time). The unit,
    // model and Tier-1 lanes drive the engine and the ledger in process; only
    // this one drives the ctl `resize` path, the PTY and the cast recorder
    // together.
    v.push(spec(
        StageId::RenderDesync,
        "render desync (a net-zero alt-screen flap an app cannot see, read by status, resizes and cast drift in a private headless aterm built this run)",
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
    // …) and 119 are predicates no cell's
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
        after_stages: Vec::new(),
    }
}

fn exclusive(id: StageId, title: impl Into<String>, lane: Lane) -> StageSpec {
    StageSpec {
        id,
        title: title.into(),
        lane,
        exclusive: true,
        after_lanes: Vec::new(),
        after_stages: Vec::new(),
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
                StageId::ExportContent,
                StageId::DeliveryTooling,
                StageId::TrustContractProbe,
                StageId::TrustAdvisory,
                StageId::LibcOracle,
                StageId::FreezeGate,
                StageId::Forge,
                StageId::ForeignCells,
                StageId::DriverBuilds,
                StageId::AtpkgTooling,
                StageId::TailDriverBuilds,
                StageId::ControlSocketSmoke,
                StageId::RedrawConformance,
                StageId::ObjcDrives,
                StageId::ForegroundHandback,
                StageId::RenderDesync,
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

    /// THE DOCTESTS START WHEN THE TEST COMPILE ENDS AND RUN BESIDE THE TEST
    /// RUN (2026-10-05). In every mode and scope that plans them they still
    /// print right after the test row, alone in [`Lane::Doctests`] (the main
    /// lane's directory), awaiting exactly the test compile: the scheduler's
    /// own rule holds them until it has finished, then starts them while the
    /// test run still waits on the driver lane, or while it runs.
    #[test]
    fn the_doctests_start_when_the_test_compile_ends_and_run_beside_the_test_run() {
        use crate::sched::{awaited, ready};
        let mut planned = 0;
        for mode in MODES {
            for scope in [
                Scope::workspace(),
                Scope::crate_only("aterm-grid"),
                Scope::changed("main", vec![], true),
            ] {
                let what = format!("{mode:?} / {}", scope.label());
                let c = ctx(mode, scope);
                let p = plan(&c);
                let at = |id| p.iter().position(|s| s.id == id);
                let Some(doc) = at(StageId::Doctests) else {
                    assert!(!mode.runs(Tier::Land), "{what}");
                    continue;
                };
                planned += 1;
                let compile = at(StageId::TestCompile).expect("the test compile");
                let test = at(StageId::Test).expect("the test run");
                assert_eq!(
                    (test, doc),
                    (compile + 1, compile + 2),
                    "{what}: the ladder"
                );
                assert_eq!(p[doc].lane, Lane::Doctests, "{what}");
                assert_eq!(
                    p.iter().filter(|s| s.lane == Lane::Doctests).count(),
                    1,
                    "{what}"
                );
                assert_eq!(lane_dir(&c, Lane::Doctests), lane_dir(&c, Lane::MainTarget));
                assert!(!p[doc].exclusive && p[doc].after_lanes.is_empty(), "{what}");
                assert_eq!(awaited(&p, doc).collect::<Vec<_>>(), [compile], "{what}");
                let none = vec![false; p.len()];
                assert!(!ready(&p, &none, &none, 0, doc), "{what}: not before it");
                let mut done = none.clone();
                done[compile] = true;
                let mut started = done.clone();
                assert!(!ready(&p, &started, &done, 0, test), "{what}: drivers");
                assert!(ready(&p, &started, &done, 0, doc), "{what}: test waiting");
                started[test] = true;
                assert!(ready(&p, &started, &done, 1, doc), "{what}: test running");
            }
        }
        assert_eq!(planned, 6);
    }

    /// THE TAIL DRIVER BUILDS DO NOT HOLD THE TEST RUN, AND THE BARRIER WAITS
    /// FOR THEM (2026-09-29). In every mode and scope that plans them they are
    /// the driver lane's LAST row before the first barrier, so lane order
    /// starts them only after every row the test run waits for; the test
    /// run's awaited set leaves them out, and nothing else in it; and every
    /// row that drives what they build sits behind the barrier.
    #[test]
    fn the_tail_driver_builds_do_not_hold_the_test_run_and_the_barrier_waits_for_them() {
        assert_eq!(NOT_AWAITED, [StageId::TailDriverBuilds]);
        let mut planned = 0;
        for mode in MODES {
            for scope in [
                Scope::workspace(),
                Scope::crate_only("aterm-grid"),
                Scope::crate_only("aterm-link"),
                Scope::changed("main", vec![], true),
            ] {
                let what = format!("{mode:?} / {}", scope.label());
                let p = plan(&ctx(mode, scope));
                let at = |id| p.iter().position(|s| s.id == id);
                let Some(tail) = at(StageId::TailDriverBuilds) else {
                    assert_eq!(at(StageId::DriverBuilds), None, "{what}: planned together");
                    continue;
                };
                planned += 1;
                let barrier = p.iter().position(|s| s.exclusive).expect("a barrier");
                assert!(tail < barrier, "{what}: the barrier waits for them");
                assert_eq!(p[tail].lane, Lane::DriverTarget, "{what}");
                assert!(
                    !p[tail].exclusive && p[tail].after_lanes.is_empty(),
                    "{what}"
                );
                assert!(
                    p[tail + 1..barrier]
                        .iter()
                        .all(|s| s.lane != Lane::DriverTarget),
                    "{what}: the lane's last row before the barrier"
                );
                let test = at(StageId::Test).expect("the test run");
                let awaited: Vec<usize> = crate::sched::awaited(&p, test).collect();
                assert!(!awaited.contains(&tail), "{what}");
                // Every OTHER driver-lane row before the barrier is still awaited.
                for (i, s) in p[..barrier].iter().enumerate() {
                    if s.lane == Lane::DriverTarget && i != tail {
                        assert!(awaited.contains(&i), "{what}: {}", s.title);
                    }
                }
                for id in [
                    StageId::ForegroundHandback,
                    StageId::RenderDesync,
                    StageId::RedrawConformance,
                    StageId::ObjcDrives,
                ] {
                    let row = at(id).unwrap_or_else(|| panic!("{what}: {id:?} planned"));
                    assert!(barrier < row, "{what}: {id:?} drives what the tail built");
                }
            }
        }
        assert!(planned >= 8, "{planned}");
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

    /// THE STAGE-SET PROOF across modes and scopes. Three rows follow their
    /// own crates: the sealed lane is aterm-link's, and the measuring tests
    /// and their release prime are aterm-conformance's. Every mode and scope
    /// plans EXACTLY the whole-tree ladder of its mode minus the rows its
    /// crates dropped — in the same order, nothing else lost. With the
    /// literal fast ladder above, the MEASURE tier and `--full`'s own stages,
    /// that pins every ladder the gate can print.
    #[test]
    fn a_scope_removes_exactly_the_lanes_whose_crates_it_dropped() {
        for mode in MODES {
            let whole = ids(&ctx(mode, Scope::workspace()));
            for scope in [
                Scope::workspace(),
                Scope::crate_only("aterm-grid"),
                Scope::crate_only("aterm-conformance"),
                Scope::changed("main", vec!["aterm-gui".into()], true),
                Scope::changed("main", vec![], true),
            ] {
                let c = ctx(mode, scope.clone());
                let expected: Vec<StageId> = whole
                    .iter()
                    .filter(|id| match id {
                        StageId::SealedLane => c.scope.includes_sealed_lane(),
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
            StageId::ConformanceRelease,
            StageId::MeasuringTests,
        ] {
            assert!(!scoped.contains(&id), "{id:?}");
        }
        let link_only = ids(&ctx(Mode::Full, Scope::crate_only("aterm-link")));
        assert!(link_only.contains(&StageId::SealedLane));
    }

    /// The measuring stage and the smokes own the machine because they judge
    /// timings; the Codex live upgrade (`--full` only) because it waits on a
    /// real Codex's idle points under deadlines while `--full`'s MainTarget
    /// tiers would otherwise compile beside it. It is LAST, so nothing waits
    /// behind it.
    #[test]
    fn only_the_stages_that_judge_timings_are_exclusive() {
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
        assert_eq!(exclusive(Mode::Fast), [StageId::ControlSocketSmoke]);
        assert_eq!(
            exclusive(Mode::Measure),
            [StageId::MeasuringTests, StageId::GuiSmoke]
        );
    }

    /// THE TESTS THE RUN SKIPS RUN ALONE, IN THE TIER THAT OWNS THEM
    /// (2026-09-26). The test run skips the measuring tests, so a plan without
    /// their stage would drop them silently: every mode that runs the MEASURE
    /// tier over aterm-conformance runs them, exclusive, after the
    /// conformance-release prime they use. (The launchd copy tests ran alone
    /// in a LAND-tier lane of their own until 2026-09-30; their deadlines are
    /// hang detectors now, and they run in the parallel test run.)
    #[test]
    fn the_measuring_tests_run_alone_in_the_measure_tier_in_every_scope() {
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
                    assert_eq!(barrier, measuring, "{what}: the first barrier");
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
        assert_eq!(dir(&c, Lane::Doctests).as_deref(), Some("/repo/target"));
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
        assert_eq!(
            dir(&c, Lane::TrustTarget).as_deref(),
            Some("/repo/target-trust")
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

    /// THE COMPILER JOBS FOLLOW THE USABLE MEMORY (2026-10-06): a host whose
    /// land tier builds one at a time also caps each build's compilers, one per
    /// workspace-build share of what is usable; a host with room keeps the
    /// count it had. The cap reaches every lane that compiles: the lanes that
    /// inherit get it through `child_env_add`, and the side lanes, whose own
    /// `CARGO_BUILD_JOBS` beats that, take it as their ceiling.
    #[test]
    fn a_degraded_host_caps_each_builds_compiler_jobs_and_a_healthy_one_does_not() {
        const GIB: u64 = crate::disk::GIB;
        // The caller exports 8, so what a build would otherwise run is not this
        // machine's core count.
        let host = |mode: Mode, usable: u64| {
            let mut c = ctx(mode, Scope::workspace());
            c.env.cargo_build_jobs = Some("8".into());
            c.with_host_memory(Some(8 * GIB))
                .with_host_usable(Some(usable))
                .with_memory_build_jobs()
        };
        let jobs_in = |env: &[(std::ffi::OsString, std::ffi::OsString)]| {
            env.iter()
                .rfind(|(k, _)| k == "CARGO_BUILD_JOBS")
                .map(|(_, v)| v.to_string_lossy().into_owned())
        };
        let degraded = host(Mode::Fast, GIB * 9 / 2);
        assert_eq!(
            jobs_in(&degraded.child_env_add).as_deref(),
            Some("2"),
            "4.5 GiB usable: two compilers"
        );
        for (label, cmd) in crate::stages::driver_build_cmds(&degraded) {
            assert_eq!(jobs_in(&cmd.envs).as_deref(), Some("2"), "{label}");
        }
        assert_eq!(
            jobs_in(&crate::stages::trust_lane_cmd(&degraded, &[]).envs).as_deref(),
            Some("2"),
            "the trust lane"
        );
        let healthy = host(Mode::Fast, GIB * 7);
        assert_eq!(
            jobs_in(&healthy.child_env_add),
            None,
            "a healthy 8 GiB host keeps the caller's count"
        );
        assert_eq!(healthy.build_jobs_ceiling(), Some(8));
        // A `--measure` run has no land-tier build, and its release build is
        // capped all the same.
        let measure = host(Mode::Measure, GIB * 9 / 2);
        assert_eq!(jobs_in(&measure.child_env_add).as_deref(), Some("2"));
        assert_eq!(
            jobs_in(&crate::stages::conformance_release_cmd(&measure).envs).as_deref(),
            Some("2"),
            "a degraded host caps the MEASURE tier's release build too"
        );
    }

    /// THE TEST POOL'S MEMORY CAP (2026-10-04): the binaries the host holds
    /// beside the builds that can run during the test run — the land tier's
    /// two besides the test compile, the doctests (beside it since
    /// 2026-10-05), and in `--full` the release build too.
    #[test]
    fn the_test_pool_is_as_wide_as_the_memory_beside_the_builds() {
        use crate::testrun::{FLOOR_JOBS, memory_jobs};
        const GIB: u64 = crate::disk::GIB;
        let fast = plan(&ctx(Mode::Fast, Scope::workspace()).with_host_memory(Some(128 * GIB)));
        let full = plan(&ctx(Mode::Full, Scope::workspace()).with_host_memory(Some(128 * GIB)));
        let measure =
            plan(&ctx(Mode::Measure, Scope::workspace()).with_host_memory(Some(128 * GIB)));
        assert_eq!(
            (builds_beside_tests(&fast), builds_beside_tests(&full)),
            (3, 4)
        );
        // The doctests are the one beside the test run that the peak does not
        // count: they await the test compile.
        assert_eq!(
            (
                concurrent_builds(&fast, Tier::Land),
                concurrent_builds(&full, Tier::Land)
            ),
            (3, 3)
        );
        assert_eq!(
            memory_jobs(Some(8 * GIB), &fast),
            FLOOR_JOBS,
            "3 held, 3 beside"
        );
        assert_eq!(memory_jobs(Some(16 * GIB), &fast), 4);
        assert_eq!(memory_jobs(Some(16 * GIB), &full), 3);
        assert_eq!(
            memory_jobs(Some(GIB), &fast),
            FLOOR_JOBS,
            "never under the floor"
        );
        assert_eq!(
            memory_jobs(None, &fast),
            FLOOR_JOBS,
            "an unread host is not a big one"
        );

        assert_eq!(
            test_pool_note(Some(8 * GIB), &fast, 8).as_deref(),
            Some(
                "verify: the test pool runs at most 2 binaries at once, beside 3 workspace build(s)"
            )
        );
        assert_eq!(
            test_pool_note(Some(16 * GIB), &fast, 8).as_deref(),
            Some(
                "verify: the test pool runs at most 4 binaries at once, beside 3 workspace build(s)"
            ),
            "a cap, not a level"
        );
        assert_eq!(
            test_pool_note(None, &fast, 8).as_deref(),
            Some("verify: the test pool runs at most 2 binaries at once")
        );
        assert_eq!(
            test_pool_note(Some(8 * GIB), &fast, 2),
            None,
            "--test-jobs at the cap"
        );
        assert_eq!(
            test_pool_note(Some(128 * GIB), &fast, 8),
            None,
            "room to spare"
        );
        assert_eq!(
            test_pool_note(Some(8 * GIB), &measure, 8),
            None,
            "no test stage"
        );
    }

    /// THE MEASURE TIER'S BUILD WAITS ONLY WHERE MEMORY RUNS OUT (2026-10-03),
    /// decided as a pure function of (RAM, plan). On the measured 8 GiB host
    /// `--full`'s land tier runs 3 workspace builds at once and the release
    /// build would be a 4th, past the 3 that host holds: the release build
    /// then waits for every land-tier build, and one line says so. On a 12 GiB
    /// host (5 held) or the 128 GiB one nothing moves and nothing is said;
    /// with memory unread the release build waits too, and the line says why. One tier alone never waits on any host, so
    /// `--measure` and the land tier keep their plans. Only scheduling moves:
    /// the rows, their order and their titles are the overlapping plan's.
    #[test]
    fn a_small_host_builds_the_measure_tier_after_the_land_tier_and_says_so() {
        const GIB: u64 = crate::disk::GIB;
        assert_eq!(builds_held(8 * GIB), 3);
        assert_eq!(builds_held(12 * GIB), 5);
        assert_eq!(builds_held(GIB), 0);
        let full = plan(&ctx(Mode::Full, Scope::workspace()).with_host_memory(Some(128 * GIB)));
        assert_eq!(concurrent_builds(&full, Tier::Land), 3);
        assert_eq!(concurrent_builds(&full, Tier::Measure), 1);

        let small = measure_overlap(Some(8 * GIB), &full);
        assert_eq!(
            small,
            MeasureOverlap::AfterLand {
                ram: 8 * GIB,
                holds: 3,
                land: 3,
                measure: 1,
            }
        );
        assert_eq!(
            small.note(false).as_deref(),
            Some(
                "verify: the MEASURE tier builds after the land tier's (10 GiB runs them together)"
            )
        );
        // The RAM, what it holds and what each tier runs: stated once, first.
        // The trust lane waits there too, so the land tier's own `--full`
        // memory lines are three.
        assert_eq!(
            memory_notes(Some(8 * GIB), None, false, &full, 2),
            [
                "verify: 8 GiB RAM holds 3 workspace build(s) at once; the land tier runs 3 and \
                 the MEASURE tier 1",
                "verify: the MEASURE tier builds after the land tier's (10 GiB runs them together)",
                "verify: the trust lane verifies after the tests and builds (12 GiB runs it beside \
                 them)",
            ]
        );
        // Linux's MemTotal for an 8 GiB box: the head states the RAM the
        // decision used, so its own figures reproduce `holds`.
        let linux = memory_notes(Some(8 * GIB - GIB * 3 / 10), None, true, &full, 2);
        assert!(
            linux[0].starts_with("verify: 7.7 GiB RAM (MemTotal) holds 2 workspace build(s)"),
            "{linux:?}"
        );
        // The needed figures are in the same units, so the box they name
        // really runs the builds together.
        assert_eq!(
            linux[1],
            "verify: the MEASURE tier builds after the land tier's (10 GiB (MemTotal) runs them \
             together)"
        );
        assert!(
            linux[2].contains("GiB (MemTotal) runs it beside them"),
            "{linux:?}"
        );
        // Memory unread: the safe, serial order, and the head says why.
        let unread = measure_overlap(None, &full);
        assert_eq!(unread, MeasureOverlap::RamUnread);
        assert_eq!(
            memory_notes(None, None, false, &full, 2)[..2],
            [
                "verify: this host's RAM could not be read",
                "verify: the MEASURE tier builds after the land tier's",
            ]
        );
        for big in [Some(12 * GIB), Some(128 * GIB)] {
            assert_eq!(
                measure_overlap(big, &full),
                MeasureOverlap::Overlaps,
                "{big:?}"
            );
            assert_eq!(measure_overlap(big, &full).note(false), None, "{big:?}");
        }
        // One tier alone overlaps nothing, however small the host.
        for mode in [Mode::Fast, Mode::Measure] {
            let p = plan(&ctx(mode, Scope::workspace()));
            assert_eq!(
                measure_overlap(Some(GIB), &p),
                MeasureOverlap::Overlaps,
                "{mode:?}"
            );
        }

        // Through `plan`: the small host's release build awaits exactly the
        // land tier's workspace builds, every one of them before the first
        // barrier; nothing else differs from the big host's plan.
        let on = |ram| plan(&ctx(Mode::Full, Scope::workspace()).with_host_memory(Some(ram)));
        let (small, big) = (on(8 * GIB), on(128 * GIB));
        assert_eq!(big, full);
        // An unread host plans exactly what the small one does.
        assert_eq!(plan(&ctx(Mode::Full, Scope::workspace())), small);
        let release = small
            .iter()
            .position(|s| s.id == StageId::ConformanceRelease)
            .expect("the release build");
        let mut waited: Vec<StageId> = crate::sched::awaited(&small, release)
            .map(|j| small[j].id)
            .collect();
        waited.sort_by_key(|id| *id as u8);
        assert_eq!(
            waited,
            [
                StageId::TestCompile,
                StageId::Tippy,
                StageId::DriverBuilds,
                StageId::TailDriverBuilds
            ]
        );
        // Equal but for the waits the memory added, on the two rows it adds
        // them to; on the big host only the doctests wait on a stage (their
        // own wait on the test compile, which every host plans alike).
        assert_eq!(
            full.iter()
                .filter(|s| !s.after_stages.is_empty())
                .map(|s| s.id)
                .collect::<Vec<_>>(),
            [StageId::Doctests]
        );
        for (a, b) in small.iter().zip(&full) {
            if matches!(a.id, StageId::ConformanceRelease | StageId::TrustAdvisory) {
                let unwaited = StageSpec {
                    after_stages: Vec::new(),
                    ..a.clone()
                };
                assert_eq!(&unwaited, b);
            } else {
                assert_eq!(a, b);
            }
        }
        // Whatever the mode and scope, the small host's plan stays
        // deadlock-free and keeps every row.
        for mode in MODES {
            for scope in [
                Scope::workspace(),
                Scope::crate_only("aterm-conformance"),
                Scope::changed("main", vec!["aterm-gui".into()], true),
            ] {
                let p = plan(&ctx(mode, scope.clone()).with_host_memory(Some(8 * GIB)));
                crate::sched::check_after_lanes(&p).expect("deadlock-free");
                assert_eq!(p.len(), plan(&ctx(mode, scope)).len());
            }
        }
    }

    /// THE TRUST LANE VERIFIES AFTER THE TESTS WHERE MEMORY RUNS OUT
    /// (2026-10-04), decided as a pure function of (RAM, plan). On the
    /// measured 8 GiB host the land tier's 3 workspace builds fill the 3 it
    /// holds, so the trust lane awaits them, the test run ([`TEST_RUN_END`],
    /// the run itself since 2026-10-05, with the rows it awaits) and the
    /// doctests beside it, and one line says so; with memory unread it waits
    /// too. A 12 GiB (5 held) or 128 GiB host keeps it at t0 and says
    /// nothing; a plan with no trust lane is unchanged.
    #[test]
    fn a_small_host_verifies_the_trust_lane_after_the_tests_and_says_so() {
        const GIB: u64 = crate::disk::GIB;
        let fast = |ram| plan(&ctx(Mode::Fast, Scope::workspace()).with_host_memory(ram));
        let big = fast(Some(128 * GIB));
        assert_eq!(concurrent_builds(&big, Tier::Land), 3);

        let small = trust_overlap(Some(8 * GIB), &big);
        assert_eq!(
            small,
            TrustOverlap::AfterTests {
                ram: 8 * GIB,
                holds: 3,
                land: 3,
                measure: 0,
                need: 10,
            }
        );
        // The RAM once, then each change: the land tier's 8 GiB run.
        assert_eq!(
            memory_notes(Some(8 * GIB), None, false, &big, 8),
            [
                "verify: 8 GiB RAM holds 3 workspace build(s) at once; the land tier runs 3",
                "verify: the trust lane verifies after the tests and builds (10 GiB runs it beside \
                 them)",
                "verify: the test pool runs at most 2 binaries at once, beside 3 workspace build(s)",
            ]
        );

        let unread = trust_overlap(None, &big);
        assert_eq!(unread, TrustOverlap::RamUnread);
        assert_eq!(
            memory_notes(None, None, false, &big, 2),
            [
                "verify: this host's RAM could not be read",
                "verify: the trust lane verifies after the tests and builds",
            ]
        );

        for ram in [Some(12 * GIB), Some(128 * GIB)] {
            assert_eq!(trust_overlap(ram, &big), TrustOverlap::Overlaps, "{ram:?}");
            assert_eq!(trust_overlap(ram, &big).note(false), None, "{ram:?}");
        }
        // No trust lane (the MEASURE tier alone): nothing to hold.
        let measure = plan(&ctx(Mode::Measure, Scope::workspace()));
        assert_eq!(trust_overlap(Some(GIB), &measure), TrustOverlap::Overlaps);
        assert_eq!(trust_overlap(None, &measure), TrustOverlap::Overlaps);

        // Through `plan`: the trust lane starts at t0 on the big host, and on
        // the small one (and an unread one) awaits exactly the land tier's
        // workspace builds, the test run with the driver-lane rows it awaits
        // (which end before it starts), and the doctests beside it — so no
        // verifier runs beside a test.
        let trust = |p: &[StageSpec]| {
            p.iter()
                .position(|s| s.id == StageId::TrustAdvisory)
                .expect("the trust lane")
        };
        let t = trust(&big);
        let none = vec![false; big.len()];
        assert!(
            crate::sched::ready(&big, &none, &none, 0, t),
            "t0 on 128 GiB"
        );
        let (small, unread) = (fast(Some(8 * GIB)), fast(None));
        assert_eq!(small, unread);
        let mut waited: Vec<StageId> = crate::sched::awaited(&small, trust(&small))
            .map(|j| small[j].id)
            .collect();
        waited.sort_by_key(|id| *id as u8);
        let mut want = vec![
            StageId::TestCompile,
            StageId::Test,
            StageId::Doctests,
            StageId::Tippy,
            StageId::AtpkgTooling,
            StageId::DriverBuilds,
            StageId::TailDriverBuilds,
        ];
        want.sort_by_key(|id| *id as u8);
        assert_eq!(waited, want);
        // TEST_RUN_END is the test run, and the doctests no longer mark its
        // end: they run beside it, in a lane of their own.
        let pos = |id| small.iter().position(|s| s.id == id).expect("planned");
        assert_eq!(TEST_RUN_END, StageId::Test);
        assert_ne!(
            small[pos(StageId::Test)].lane,
            small[pos(StageId::Doctests)].lane
        );
        // Awaiting the test run is accepted only with what it awaits: drop
        // one of those and the plan is refused.
        let mut short = small.clone();
        let trust_row = trust(&short);
        short[trust_row]
            .after_stages
            .retain(|id| *id != StageId::AtpkgTooling);
        assert!(crate::sched::check_after_lanes(&short).is_err());
        crate::sched::check_after_lanes(&small).expect("deadlock-free");
        // Only the trust row moves; on the big host it waits on nothing.
        for (a, b) in small.iter().zip(&big) {
            if a.id == StageId::TrustAdvisory {
                let unwaited = StageSpec {
                    after_stages: Vec::new(),
                    ..a.clone()
                };
                assert_eq!(&unwaited, b);
            } else {
                assert_eq!(a, b);
            }
        }

        // `--full`: the release build counts where it overlaps the land
        // tier's builds. 10 GiB holds 4 and lets the tiers overlap (3 + 1),
        // so the trust lane waits; 12 GiB holds 5 and it does not.
        let full = |ram| plan(&ctx(Mode::Full, Scope::workspace()).with_host_memory(ram));
        let full_big = full(Some(128 * GIB));
        assert_eq!(
            trust_overlap(Some(10 * GIB), &full_big),
            TrustOverlap::AfterTests {
                ram: 10 * GIB,
                holds: 4,
                land: 3,
                measure: 1,
                need: 12,
            }
        );
        assert_eq!(
            trust_overlap(Some(12 * GIB), &full_big),
            TrustOverlap::Overlaps
        );
        // On 8 GiB the release build waits for the land tier's builds
        // (`measure_overlap`) and the trust lane awaits it as well, so no
        // verifier runs beside the fat-LTO build either; on 10 GiB, where
        // the release build waits on nothing, the same set. (The sealed rung
        // is one of the driver-lane rows the test run awaits in `--full`.)
        for ram in [Some(8 * GIB), Some(10 * GIB), None] {
            let p = full(ram);
            crate::sched::check_after_lanes(&p).expect("deadlock-free");
            let mut waited: Vec<StageId> = crate::sched::awaited(&p, trust(&p))
                .map(|j| p[j].id)
                .collect();
            waited.sort_by_key(|id| *id as u8);
            let mut want_full = want.clone();
            want_full.push(StageId::ConformanceRelease);
            want_full.push(StageId::SealedLane);
            want_full.sort_by_key(|id| *id as u8);
            assert_eq!(waited, want_full, "{ram:?}");
        }
        let none = vec![false; full_big.len()];
        assert!(
            crate::sched::ready(&full_big, &none, &none, 0, trust(&full_big)),
            "t0 on 128 GiB --full"
        );
    }

    /// WIRED MEMORY PAST A HEALTHY HOST'S SERIALISES THE LAND TIER'S BUILDS
    /// (2026-10-06), decided as a pure function of (RAM, usable, plan). The
    /// measured degraded host — 8 GiB with 228,350 16 KiB pages wired — builds
    /// one at a time and one line says so; a borderline one (up to 3 GiB
    /// wired) loses one build of overlap, not all of it; a healthy 8 GiB host
    /// (up to 2.5 GiB wired), an unread one and a host already short on
    /// physical RAM keep their plans, line for line.
    #[test]
    fn wired_memory_builds_the_land_tier_one_at_a_time_and_says_so() {
        const GIB: u64 = crate::disk::GIB;
        let fast = |ram, usable| {
            plan(
                &ctx(Mode::Fast, Scope::workspace())
                    .with_host_memory(ram)
                    .with_host_usable(usable),
            )
        };
        let wired = 228_350 * 16_384;
        let degraded = 8 * GIB - wired;
        let base = fast(Some(8 * GIB), None);

        // The decision.
        assert_eq!(build_memory(Some(8 * GIB), None), Some(8 * GIB));
        assert_eq!(build_memory(None, Some(GIB)), None);
        assert_eq!(build_memory(Some(8 * GIB), Some(6 * GIB)), Some(8 * GIB));
        let healthy = 8 * GIB - WIRED_ALLOWANCE_BYTES;
        assert_eq!(build_memory(Some(8 * GIB), Some(healthy)), Some(8 * GIB));
        for (ram, usable, want) in [
            (Some(8 * GIB), None, LandBuilds::Together),
            (None, None, LandBuilds::Together),
            (None, Some(degraded), LandBuilds::Together),
            (Some(8 * GIB), Some(6 * GIB), LandBuilds::Together),
            (Some(8 * GIB), Some(healthy), LandBuilds::Together),
            // Past the allowance: two at once while 5 GiB is usable.
            (Some(8 * GIB), Some(healthy - 1), LandBuilds::TwoAtATime),
            (Some(8 * GIB), Some(5 * GIB), LandBuilds::TwoAtATime),
            (Some(8 * GIB), Some(5 * GIB - 1), LandBuilds::OneAtATime),
            (Some(8 * GIB), Some(degraded), LandBuilds::OneAtATime),
            // Short on physical RAM already: wired memory within it moves
            // nothing the RAM had not.
            (Some(6 * GIB), Some(5 * GIB), LandBuilds::Together),
            // Plenty left beside the wired memory.
            (Some(16 * GIB), Some(10 * GIB), LandBuilds::Together),
        ] {
            assert_eq!(land_builds(ram, usable, &base), want, "{ram:?} {usable:?}");
        }

        // The healthy and unread hosts: the plan and the lines are the
        // physical figure's.
        for usable in [None, Some(6 * GIB), Some(healthy)] {
            assert_eq!(fast(Some(8 * GIB), usable), base, "{usable:?}");
            assert_eq!(
                memory_notes(Some(8 * GIB), usable, false, &base, 8),
                memory_notes(Some(8 * GIB), None, false, &base, 8),
                "{usable:?}"
            );
        }

        // The degraded host: one line names the figure and the remedy, then
        // each change the smaller RAM forces.
        let small = fast(Some(8 * GIB), Some(degraded));
        assert_eq!(
            memory_notes(Some(8 * GIB), Some(degraded), false, &small, 8),
            [
                "verify: 4.5 GiB of 8 GiB is usable (3.5 GiB wired) — the land tier builds one \
                 at a time; a reboot frees wired memory",
                "verify: the trust lane verifies after the tests and builds (10 GiB runs it beside \
                 them)",
                "verify: the test pool runs at most 2 binaries at once, beside 2 workspace build(s)",
            ]
        );
        // Linux's MemAvailable reads as memory in use, not wired.
        let linux = memory_notes(Some(8 * GIB), Some(4 * GIB), true, &small, 8);
        assert_eq!(
            linux[0],
            "verify: 4 GiB of 8 GiB (MemTotal) is usable (4 GiB in use) — the land tier builds \
             one at a time; quitting other programs frees memory"
        );
        // Wired memory that leaves every build room: stated, nothing serial.
        let big = fast(Some(16 * GIB), Some(10 * GIB));
        assert_eq!(
            memory_notes(Some(16 * GIB), Some(10 * GIB), false, &big, 2),
            Vec::<String>::new(),
            "nothing changes, so nothing is said"
        );
        let full_big = plan(
            &ctx(Mode::Full, Scope::workspace())
                .with_host_memory(Some(16 * GIB))
                .with_host_usable(Some(7 * GIB)),
        );
        assert_eq!(
            memory_notes(Some(16 * GIB), Some(7 * GIB), false, &full_big, 8)[0],
            "verify: 7 GiB of 16 GiB is usable (9 GiB wired) and holds 3 workspace build(s) at \
             once; the land tier runs 3 and the MEASURE tier 1"
        );

        // The chain: each land-tier build awaits every earlier one, the tail
        // driver row through its lane; nothing else but the trust row moves.
        let pos = |p: &[StageSpec], id| p.iter().position(|s| s.id == id).expect("planned");
        let waits = |id| small[pos(&small, id)].after_stages.clone();
        assert_eq!(waits(StageId::TestCompile), []);
        assert_eq!(waits(StageId::DriverBuilds), [StageId::TestCompile]);
        assert_eq!(waits(StageId::TailDriverBuilds), []);
        assert_eq!(
            waits(StageId::Tippy),
            [
                StageId::TestCompile,
                StageId::DriverBuilds,
                StageId::TailDriverBuilds
            ]
        );
        for (a, b) in small.iter().zip(&base) {
            if !LAND_BUILD_ORDER.contains(&a.id) {
                assert_eq!(a, b);
            }
        }

        // The borderline host: tippy awaits the test compile and nothing
        // else moves; one line says so, and two land builds still run beside
        // the tests (tippy and the tail driver row).
        let two = fast(Some(8 * GIB), Some(5 * GIB));
        let waits2 = |id| two[pos(&two, id)].after_stages.clone();
        assert_eq!(waits2(StageId::Tippy), [StageId::TestCompile]);
        for id in [StageId::DriverBuilds, StageId::TailDriverBuilds] {
            assert_eq!(waits2(id), base[pos(&base, id)].after_stages, "{id:?}");
        }
        assert_eq!(
            memory_notes(Some(8 * GIB), Some(5 * GIB), false, &two, 8),
            [
                "verify: 5 GiB of 8 GiB is usable (3 GiB wired) — the land tier builds two at a \
                 time, tippy after the test compile; a reboot frees wired memory",
                "verify: the trust lane verifies after the tests and builds (10 GiB runs it beside \
                 them)",
                "verify: the test pool runs at most 2 binaries at once, beside 3 workspace build(s)",
            ]
        );

        // The scheduler accepts both chains, and in every interleaving it
        // reaches — finishing the oldest running stage, the newest, or a
        // scrambled one — no more land-tier builds than decided run at once
        // and the run ends.
        for (mode, usable, most) in [
            (Mode::Fast, degraded, 1),
            (Mode::Full, degraded, 1),
            (Mode::Fast, 5 * GIB, 2),
            (Mode::Full, 5 * GIB, 2),
        ] {
            let p = plan(
                &ctx(mode, Scope::workspace())
                    .with_host_memory(Some(8 * GIB))
                    .with_host_usable(Some(usable)),
            );
            crate::sched::check_after_lanes(&p).expect("deadlock-free");
            for pick in 0..3_u32 {
                let n = p.len();
                let (mut started, mut done) = (vec![false; n], vec![false; n]);
                let mut running: Vec<usize> = Vec::new();
                let mut seed = 0x9e37_79b9_u32;
                while done.iter().any(|d| !d) {
                    while let Some(i) =
                        (0..n).find(|&i| crate::sched::ready(&p, &started, &done, running.len(), i))
                    {
                        started[i] = true;
                        running.push(i);
                    }
                    let builds = running
                        .iter()
                        .filter(|&&r| {
                            p[r].id.tier() == Tier::Land && WORKSPACE_BUILDS.contains(&p[r].id)
                        })
                        .count();
                    assert!(builds <= most, "{mode:?}: {builds} land builds at once");
                    assert!(!running.is_empty(), "{mode:?}: deadlock");
                    seed ^= seed << 13;
                    seed ^= seed >> 17;
                    seed ^= seed << 5;
                    let k = match pick {
                        0 => 0,
                        1 => running.len() - 1,
                        _ => seed as usize % running.len(),
                    };
                    done[running.remove(k)] = true;
                }
            }
        }
    }
}
