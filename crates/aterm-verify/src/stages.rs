// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The stages themselves.
//!
//! Every stage that shelled out still shells out to the SAME command with the
//! same arguments and the same environment. The argv of each one is built by a
//! small pure function below, so the port is checked by unit tests rather than by
//! a reviewer diffing two languages — including the details that are easy to lose
//! and expensive to lose: tippy's separate `CARGO_TARGET_DIR` and
//! `TRUST_NO_MIGRATE_WARN`, the doc-driver rule on the doc-running stages
//! (`DocDriver` — private to this module, so named rather than linked:
//! `RUSTDOC=<stage2>/trustdoc` when the stage2 carries it,
//! the caller's own export or the PATH farm link otherwise, a diagnosis when
//! nothing exists), and `--unverified` on every driver invocation (naming the
//! lane is the point: `targo` REFUSES a bare verb precisely so a gate cannot be
//! quietly unverified).

use crate::exec::{self, Capture, Cmd};
use crate::ladder::{Outcome, Report, Severity};
use crate::plan::{Lane, StageId, StageSpec, lane_dir};
use crate::scope::Scope;
#[cfg(unix)]
use crate::smoke_stages;
use crate::{Ctx, have_on_path, is_executable_file};

/// Dispatch one stage.
#[must_use]
pub fn run_stage(ctx: &Ctx, spec: &StageSpec) -> Report {
    let mut r = Report::new(spec.title.clone());
    match spec.id {
        StageId::TestCompile => test_compile(ctx, &mut r),
        StageId::Test => test(ctx, &mut r),
        StageId::DeadlineTests => deadline_tests(ctx, &mut r),
        StageId::MeasuringTests => measuring_tests(ctx, &mut r),
        StageId::Doctests => doctests(ctx, &mut r),
        StageId::SealedLane => sealed_lane(ctx, &mut r),
        StageId::Tippy => tippy(ctx, &mut r),
        StageId::Formatting => formatting(ctx, &mut r),
        StageId::GrepGuards => grep_guards(ctx, &mut r),
        StageId::DeliveryTooling => delivery_tooling(ctx, &mut r),
        StageId::AtpkgTooling => atpkg_tooling(ctx, &mut r),
        StageId::TrustContractProbe => trust_contract_probe(ctx, &mut r),
        StageId::StartCompare => start_compare(ctx, &mut r),
        StageId::LibcOracle => libc_oracle(ctx, &mut r),
        StageId::FreezeGate => freeze_gate(ctx, &mut r),
        StageId::DriverBuilds => driver_builds(ctx, &mut r),
        StageId::ConformanceRelease => conformance_release(ctx, &mut r),
        #[cfg(unix)]
        StageId::ControlSocketSmoke => smoke_stages::control_socket_smoke(ctx, &mut r),
        #[cfg(unix)]
        StageId::GuiSmoke => smoke_stages::gui_typing_smoke(ctx, &mut r),
        // The smokes drive a unix control socket, and the ladder is a unix
        // program; off unix the rows are named.
        #[cfg(not(unix))]
        StageId::ControlSocketSmoke | StageId::GuiSmoke => {
            r.skip(format!("{} (unix only)", spec.title));
        }
        StageId::RedrawConformance => redraw_conformance(ctx, &mut r),
        StageId::ObjcClassAudit => objc_class_audit(ctx, &mut r),
        StageId::ObjcImeDrive => objc_ime_drive(ctx, &mut r),
        StageId::ObjcToolbarDrive => objc_toolbar_drive(ctx, &mut r),
        StageId::ObjcWindowDrive => objc_window_drive(ctx, &mut r),
        StageId::ObjcEventDrive => objc_event_drive(ctx, &mut r),
        StageId::ObjcBoundDrive => objc_bound_drive(ctx, &mut r),
        StageId::ForegroundHandback => foreground_handback(ctx, &mut r),
        StageId::KaniFloor => kani_floor(ctx, &mut r),
        StageId::CrossCells => cross_cells(ctx, &mut r),
        StageId::Forge => forge_gate(ctx, &mut r),
        StageId::ForeignCells => foreign_cells(ctx, &mut r),
        StageId::CodexLiveUpgrade => codex_live_upgrade(ctx, &mut r),
    }
    r
}

// ---------------------------------------------------------------------------
// The argv builders. Pure, so the port is a test and not a promise.
// ---------------------------------------------------------------------------

/// `targo --unverified test <scope> --no-fail-fast --no-run` — the test
/// compile row's one child, and the test stage's FIRST (a fingerprint check by
/// then): compile every test target and run nothing.
///
/// `--no-fail-fast` IS THE COVERAGE FLAG, the exact analogue of `--keep-going`
/// on [`tippy_args`] below, and for the same reason: without it cargo stops the
/// moment ONE test binary fails, so the stage reports on a PREFIX of the
/// workspace while reading like a statement about all of it. MEASURED
/// 2026-09-10: `aterm-conformance` sorts early and its paint rows are
/// load-sensitive, so three consecutive gates (0.79, 0.80, and the first
/// re-gate) reported on 41 of ~319 test binaries. Everything behind it —
/// including `aterm-forge`'s six baseline tests, which had been RED for two
/// days from a commit that had already landed — never ran, and their silence
/// read as green. An absence of failure is not a pass.
///
/// It widens what the stage SEES and softens nothing: cargo still exits
/// non-zero when any test failed, so `Report::decide` reaches the same FAIL it
/// always did — only now with the other 278 binaries' results printed beside
/// the first one's.
///
/// SPLIT FROM ONE CHILD ON 2026-09-13, with the same flags, scope and
/// environment on both halves. The dry unit graphs (`targo --unverified test
/// --workspace --no-fail-fast [--no-run | --tests] --config
/// profile.dev.build-override.debug=2 --unit-graph -Z unstable-options
/// --offline`, on 07a76fca7) show `--no-run` compiles exactly the units the
/// single child did (1131 = 1131, same keys), so nothing it proved is lost.
///
/// ASKED FOR JSON since 2026-09-26 (`--message-format=json-render-diagnostics`):
/// diagnostics still print as they always did, and cargo's `compiler-artifact`
/// messages name every test executable with its target — how the test run
/// knows each binary's re-run spec ([`crate::testrun`]). The JSON lines are
/// taken out of what the ladder shows ([`crate::testrun::take_test_executables`]).
/// The message format is not part of cargo's fingerprint, so the compile
/// builds exactly what it built before.
#[must_use]
pub fn test_compile_args(scope: &Scope) -> Vec<String> {
    let mut a = test_base_args(scope);
    a.push("--no-run".to_string());
    a.push("--message-format=json-render-diagnostics".to_string());
    a
}

/// `targo --unverified test <scope> --no-fail-fast --tests` — the SECOND
/// child, run only when the first compiled. `--tests` runs every lib, bin and
/// integration test target and no doctests: the doctests stage is their only
/// runner. Graph-measured on 07a76fca7: this selection adds 0 units to the
/// `--no-run` graph and keeps all 500 test-mode units; what it leaves out is
/// the 86 doctest units and 78 example/extra-crate-type build units, which
/// run nothing here. Cargo runs no test after a compile error, so skipping
/// this child after a failed compile decides what the single child decided.
///
/// It SKIPS [`MEASURING_TESTS`] and [`DEADLINE_TESTS`] — libtest's own
/// `--skip`, handed to every test binary after `--` — and [`measuring_args`]
/// and [`deadline_args`] run exactly those, each alone, from the targets they
/// live in.
///
/// SINCE 2026-09-26 cargo runs it with a RECORDING runner
/// ([`crate::testrun::recording_cmd`]) — this argv plus one `--config` — and the
/// gate runs the binaries it recorded, `--test-jobs` at a time. This argv, as
/// is, is what runs whenever the recording does not add up.
#[must_use]
pub fn test_run_args(scope: &Scope) -> Vec<String> {
    let mut a = test_base_args(scope);
    a.push("--tests".to_string());
    a.push("--".to_string());
    for name in MEASURING_TESTS.iter().chain(&DEADLINE_TESTS) {
        a.push("--skip".to_string());
        a.push((*name).to_string());
    }
    a
}

/// THE TESTS THAT MEASURE (2026-09-23; the MEASURE tier's since 2026-09-26):
/// libtest name filters for the paint and spin matrices (`aterm-conformance`),
/// under a `measuring` module, which judge the RELEASE artifact's frame timings
/// on a live window. (`atpkg`'s untracked staging, laying and view lanes were a
/// second family until they were deleted on 2026-09-24.)
///
/// Until this list they ran inside the parallel test run, which therefore had
/// to wait for every side lane and for tippy's whole-workspace compile before it
/// could start (a paint take's driver was measured descheduled for 50 ms beside
/// tippy, 2026-09-18) — and still went red when a peer's build loaded the box.
/// From 2026-09-23 the test run skipped them and an exclusive `measuring tests`
/// stage ran them inside the merge contract; since 2026-09-26 that stage is the
/// MEASURE tier's (`--measure`, `--full`), and the merge contract runs none of
/// them.
///
/// THE SPLIT IS EXACT BECAUSE THE MODULES ARE PINNED. The test run skips each
/// filter everywhere; each filtered stage runs it only in the targets that
/// hold it — `measuring` modules in aterm-conformance's `paint` and `spin`
/// integration tests, `launchd_copy_tests` in aterm-update's lib — and
/// `every_skipped_module_is_declared_where_its_stage_looks` below refuses a
/// module named to match a filter anywhere else, where the run would skip it
/// and no stage would select it.
pub const MEASURING_TESTS: [&str; 1] = ["measuring::"];

/// The targets [`MEASURING_TESTS`] live in (`--test` names, in every selected
/// package that has one: only aterm-conformance does).
pub const MEASURING_TARGETS: [&str; 4] = ["--test", "paint", "--test", "spin"];

/// THE TESTS WITH SECONDS-LONG DEADLINES (2026-09-26): `aterm-update`'s launchd
/// copy tests, whose deadlines are one to five seconds. They decide
/// CORRECTNESS — the ty models of the copy's deadline, publication and reaper,
/// and the real wrapper's one-shot fence — so they stay in the merge contract,
/// and they run alone in the LAND tier's exclusive `deadline tests` stage.
pub const DEADLINE_TESTS: [&str; 1] = ["launchd_copy_tests::"];

/// The targets [`DEADLINE_TESTS`] live in: library unit tests.
pub const DEADLINE_TARGETS: [&str; 1] = ["--lib"];

/// `targo --unverified test <scope> --no-fail-fast <targets> -- <filters>` —
/// the test run's own PACKAGE selection, narrowed to the targets that hold
/// the tests it skipped and filtered to exactly them.
///
/// THE SCOPE IS NOT NARROWED, measured 2026-09-27 on the unit graphs: cargo
/// resolves features over the selected packages, so `-p aterm-update --lib`
/// has 7 units the `--workspace` test compile does not (`serde`, `serde_core`
/// and `cc` at other features, and everything above them relinks) and `-p
/// aterm-conformance --test paint --test spin` 26 — a per-package child would
/// recompile inside an EXCLUSIVE stage. With the run's own selection every
/// unit is the test compile's (`--workspace --lib --test paint --test spin`:
/// 523 of 523), and the target filter still cuts the binaries these stages
/// launch from the test run's 652 to 91 (89 libs, then paint and spin).
fn filtered_test_args(scope: &Scope, targets: &[&str], filters: &[&str]) -> Vec<String> {
    let mut a = test_base_args(scope);
    a.extend(targets.iter().map(|s| (*s).to_string()));
    a.push("--".to_string());
    a.extend(filters.iter().map(|s| (*s).to_string()));
    a
}

/// `targo --unverified test <scope> --no-fail-fast --test paint --test spin --
/// measuring::` — the MEASURE tier's exclusive stage's test child. In a
/// `--full` run the compile is the test stage's; a `--measure` run compiles
/// here.
#[must_use]
pub fn measuring_args(scope: &Scope) -> Vec<String> {
    filtered_test_args(scope, &MEASURING_TARGETS, &MEASURING_TESTS)
}

/// `targo --unverified test <scope> --no-fail-fast --lib --
/// launchd_copy_tests::` — the LAND tier's exclusive `deadline tests` stage's
/// one child, after the test stage's compile.
#[must_use]
pub fn deadline_args(scope: &Scope) -> Vec<String> {
    filtered_test_args(scope, &DEADLINE_TARGETS, &DEADLINE_TESTS)
}

fn test_base_args(scope: &Scope) -> Vec<String> {
    let mut a = vec!["--unverified".to_string(), "test".to_string()];
    a.extend(scope.args());
    a.push("--no-fail-fast".to_string());
    a
}

/// `targo --unverified test --doc <scope> --no-fail-fast` — run explicitly,
/// because the unit
/// stage's `targo test` can skip documentation examples when scoped or under a
/// nextest-style runner, and doctests then rot silently. `--no-fail-fast` for
/// the reason [`test_compile_args`] gives: one crate's failing doctest must not
/// hide every crate cargo had not reached yet.
///
/// THE ONLY DOCTEST RUNNER since 2026-09-13: the test stage runs `--tests`.
/// Graph-measured on 07a76fca7 with the `[profile.dev.build-override]` pin,
/// this graph adds 3 units to the test compile, all doctest (89 crates, a
/// superset of the 86 the old test child ran); without the pin it adds 117
/// build units, which is what the pin is for.
#[must_use]
pub fn doctest_args(scope: &Scope) -> Vec<String> {
    let mut a = vec![
        "--unverified".to_string(),
        "test".to_string(),
        "--doc".to_string(),
    ];
    a.extend(scope.args());
    a.push("--no-fail-fast".to_string());
    a
}

/// `targo --unverified build -q -p aterm-gui -p aterm-ctl` — the two binaries
/// the smokes and the sealed rung drive.
#[must_use]
pub fn smoke_build_args() -> Vec<String> {
    [
        "--unverified",
        "build",
        "-q",
        "-p",
        "aterm-gui",
        "-p",
        "aterm-ctl",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// `targo --unverified test -p aterm-link --features sealed --test two_nodes_sealed --no-fail-fast`
///
/// The sealed cross-host rung. `tests/two_nodes_sealed.rs` is `#![cfg(feature =
/// "sealed")]`, so the workspace test stage compiles it to NOTHING, and the one
/// test covering the vendored astream-aead ran only when somebody typed this by
/// hand (audit 2026-09-12). The rung's second child, in the driver lane behind
/// the `aterm-gui` build it drives: see [`sealed_lane_cmds`].
#[must_use]
pub fn sealed_lane_args() -> Vec<String> {
    [
        "--unverified",
        "test",
        "-p",
        "aterm-link",
        "--features",
        "sealed",
        "--test",
        "two_nodes_sealed",
        "--no-fail-fast",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// `targo --unverified build -q -p atpkg` — the binary the end-to-end pack
/// drives, built in the driver lane as that stage's first child and handed to
/// the suite ([`atpkg_pack_cmd`]): the script's own fallbacks are a PREVIOUS
/// run's artifact.
#[must_use]
pub fn atpkg_build_args() -> Vec<String> {
    ["--unverified", "build", "-q", "-p", "atpkg"]
        .into_iter()
        .map(String::from)
        .collect()
}

/// `targo --unverified build --locked --release -p aterm` — the RELEASE
/// artifact the paint and spin suites judge, the argv their own helper
/// (`crates/aterm-conformance/tests/support/mod.rs`, `release_bin`) builds when
/// run by hand. The gate builds it itself and HANDS it to them
/// ([`conformance_release_binary`]), so nothing here has to match the helper's
/// fingerprint.
#[must_use]
pub fn conformance_release_args() -> Vec<String> {
    [
        "--unverified",
        "build",
        "--locked",
        "--release",
        "-p",
        "aterm",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// `<tippy> <scope> --all-targets --keep-going -- -D warnings`
///
/// `--keep-going` IS THE COVERAGE FLAG, not a tuning knob. Without it cargo
/// stops scheduling new units the moment one fails, so under `-D warnings` the
/// FIRST crate with a finding ends the run and every crate cargo had not
/// reached yet goes unlinted — the lint reports a PREFIX of the workspace while
/// reading like a statement about all of it. MEASURED on this tree 2026-08-26:
/// the aborting form reported 3 findings in `atpkg`; the same tree under
/// `--keep-going` reported 9, in `atpkg`, `aterm-conformance` and `aterm-gui`.
/// That is also the mechanism behind the three "fixed it — no, here is another
/// one" rounds of 2026-08-11 (see `crates/xtask/src/gate.rs`): each round
/// fixed the crate that happened to abort first and revealed the next.
///
/// It does NOT make coverage unconditional: a member whose DEPENDENCY fails to
/// compile still cannot be linted, because there is no metadata to lint it
/// against. `gate lint` says so out loud rather than implying otherwise, and
/// the differential verdict never inherits such a row from main (2026-09-27,
/// fourth review: [`crate::differential::lint_reached_every_unit`]) — its
/// output is the same whatever a branch did to the crates it left unlinted.
#[must_use]
pub fn tippy_args(scope: &Scope) -> Vec<String> {
    let mut a = scope.args();
    a.extend(
        ["--all-targets", "--keep-going", "--", "-D", "warnings"]
            .into_iter()
            .map(String::from),
    );
    a
}

/// The workspace's `required-features` targets, as the `(package, feature)`
/// pairs that switch them on.
///
/// SIX TARGETS HANG OFF THESE THREE PAIRS, and `--all-targets` builds none of
/// them: cargo skips a target whose `required-features` are off, silently and
/// without a word in its output. So the main tippy pass — `--workspace
/// --all-targets` — never compiled `aterm-gui`'s three `bench-support`
/// benches, its `control-conformance` bin, or `aterm-scrollback`'s two
/// `disk-tier` benches. That is not a theoretical hole: a broken bench build
/// lived in it for four days in August 2026, and the campaign's count gates
/// and reach guards LIVE in those benches, so an unbuilt bench is a gate that
/// silently stopped existing.
///
/// The table is checked against the tree by
/// `the_gated_feature_table_covers_every_required_features_target` below, so a
/// seventh gated target cannot be added without either extending this or
/// reddening that test. An entry no manifest declares any more fails the
/// `--full` lint itself: cargo refuses a feature a package does not have.
pub const GATED_LINT_FEATURES: [(&str, &str); 3] = [
    ("aterm-gui", "bench-support"),
    ("aterm-gui", "control-conformance"),
    ("aterm-scrollback", "disk-tier"),
];

/// `<tippy> -p <pkg>… --features <pkg/feat>,… --all-targets --keep-going --
///  -D warnings` — the SECOND pass, the one that reaches the targets
/// [`tippy_args`] cannot.
///
/// A separate invocation rather than a flag on the first, because the features
/// belong to specific packages: turning them on for the whole workspace is not
/// something cargo offers, and `-p <pkg> --features <pkg>/<feat>` is. Its cost
/// is one re-lint of the two named packages against a wider feature set — and
/// CORRECTED 2026-09-13, the dependencies below them are NOT all cache hits
/// from the first pass: the speed round's unit graphs count 139 unit variants
/// this feature set resolves that the first pass never builds.
///
/// `None` when the scope selects neither package — under `--scope aterm-core`
/// there is no gated target to reach, and running the pass anyway would compile
/// two crates the run had deliberately narrowed away.
#[must_use]
pub fn tippy_gated_args(scope: &Scope) -> Option<Vec<String>> {
    let mut packages: Vec<&str> = Vec::new();
    let mut features: Vec<String> = Vec::new();
    for (pkg, feat) in GATED_LINT_FEATURES {
        if !scope.includes_crate(pkg) {
            continue;
        }
        if !packages.contains(&pkg) {
            packages.push(pkg);
        }
        features.push(format!("{pkg}/{feat}"));
    }
    if packages.is_empty() {
        return None;
    }
    let mut a: Vec<String> = packages
        .iter()
        .flat_map(|p| ["-p".to_string(), (*p).to_string()])
        .collect();
    a.push("--features".to_string());
    a.push(features.join(","));
    a.extend(
        ["--all-targets", "--keep-going", "--", "-D", "warnings"]
            .into_iter()
            .map(String::from),
    );
    Some(a)
}

/// `targo --unverified run -q -p xtask -- gate <name>`
#[must_use]
pub fn xtask_gate_args(gate: &str) -> Vec<String> {
    xtask_gate_args_with(gate, &[])
}

/// [`xtask_gate_args`] plus flags for the verb. Separate so the flags a stage
/// passes are visible at the stage rather than buried in a string.
#[must_use]
pub fn xtask_gate_args_with(gate: &str, flags: &[&str]) -> Vec<String> {
    let mut a: Vec<String> = [
        "--unverified",
        "run",
        "-q",
        "-p",
        "xtask",
        "--",
        "gate",
        gate,
    ]
    .into_iter()
    .map(String::from)
    .collect();
    a.extend(flags.iter().map(|f| String::from(*f)));
    a
}

/// `targo --unverified build --manifest-path tools/freeze-safety-gate/Cargo.toml`
#[must_use]
pub fn freeze_gate_args() -> Vec<String> {
    [
        "--unverified",
        "build",
        "--manifest-path",
        "tools/freeze-safety-gate/Cargo.toml",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// The crates carrying a Kani BMC floor, in the order the stage runs them.
/// `aterm-containment` joined 2026-09-25 (its six policy-mapping harnesses
/// prove in seconds). `aterm-policy` cannot: the bundled trust-mc does not build
/// its `serde` build scripts, so its harnesses were retired for exhaustive unit
/// tests.
pub const KANI_CRATES: [&str; 4] = [
    "aterm-parser",
    "aterm-render",
    "aterm-uds",
    "aterm-containment",
];

/// The exact command one Kani floor run spawns. `KANI_CRATE` selects which
/// crate's proofs the script drives; lose it and every iteration of the loop
/// runs the same default, so every other crate goes unproven while the
/// ladder still prints a green row for each. `TRUST_MC_SYSROOT` / `AY_BIN_DIR`
/// carry what THIS process resolved ([`trust_mc_sysroot`], [`ay_bin_dir`]) so
/// the script and the availability decision above it can never disagree
/// about which trust-mc is being driven.
fn kani_cmd(
    gate: &std::path::Path,
    krate: &str,
    mc_root: &std::path::Path,
    ay_dir: &std::path::Path,
) -> Cmd {
    Cmd::new(gate)
        .env("KANI_CRATE", krate)
        .env("TRUST_MC_SYSROOT", mc_root)
        .env("AY_BIN_DIR", ay_dir)
        .capture(Capture::Emit)
}

/// Where trust-mc lives — the same order `scripts/verify-kani-proofs.sh` uses:
/// `$TRUST_MC_SYSROOT` (an explicit development location, never fallen back from),
/// else `<atpkg prefix>/store/trust-mc/current` — what `aterm pkg install trust-mc`
/// lays down, reported even when absent so the availability check names the place
/// the remedy fills. The managed bundle ships no `cargo-trust-mc` name — the script
/// derives that symlink OUTSIDE the store, which is tree_root-attested and immutable.
/// No build tree is probed: `$HOME/trust/first-party` was retired 2026-08-29, and a
/// stale checkout there shadowed the store until 2026-09-24.
///
/// The store's `current` is RESOLVED when it is there (2026-09-24): it is a link
/// every `aterm pkg update` re-points, the Kani stage runs the script once per
/// crate in [`KANI_CRATES`] with this one value, and a driver started through the
/// unresolved spelling finds its siblings by that spelling — so an update landing
/// between two crates (or inside one) split the floor across two trust-mc builds.
/// The build-numbered directory cannot move; an absent store stays the unresolved
/// place the remedy fills, for the diagnostic.
#[must_use]
pub fn trust_mc_sysroot(env: &crate::EnvSnapshot) -> std::path::PathBuf {
    env.trust_mc_sysroot.clone().unwrap_or_else(|| {
        let live = crate::toolchain::atpkg_prefix(&env.home, env.xdg_config_home.as_deref())
            .join("store/trust-mc/current");
        std::fs::canonicalize(&live).unwrap_or(live)
    })
}

/// Where the `ay` solver lives, in the script's order: `$AY_BIN_DIR`, else the
/// atpkg shim dir (`<prefix>/bin`, where `aterm pkg install ay` shims it).
#[must_use]
pub fn ay_bin_dir(env: &crate::EnvSnapshot) -> std::path::PathBuf {
    env.ay_bin_dir.clone().unwrap_or_else(|| {
        crate::toolchain::atpkg_prefix(&env.home, env.xdg_config_home.as_deref()).join("bin")
    })
}

/// The redraw harness's target name — the `[[bin]]`, the built file and the
/// argv below all have to agree, so they read it from here.
pub const REDRAW_CONFORMANCE_BIN: &str = "aterm-redraw-conformance";

/// `targo --unverified build -q -p aterm-gui --features control-conformance
///  --bin aterm-redraw-conformance`
///
/// The feature is the whole reason the argv is spelled out and tested: it is
/// `required-features` on the target, so without it cargo silently builds
/// NOTHING and the stage would gate on a binary from some previous run.
#[must_use]
pub fn redraw_conformance_build_args() -> Vec<String> {
    [
        "--unverified",
        "build",
        "-q",
        "-p",
        "aterm-gui",
        "--features",
        "control-conformance",
        "--bin",
        REDRAW_CONFORMANCE_BIN,
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// The live-class auditor's target name — the `[[example]]`, the built file and
/// the argv below all have to agree, so they read it from here.
pub const OBJC_CLASS_AUDIT_EXAMPLE: &str = "objc_live_class_audit";

/// `targo --unverified build -q -p aterm-gui --example objc_live_class_audit`
///
/// NO `required-features`, deliberately, unlike the redraw harness beside it:
/// the auditor needs nothing aterm-gui does not already compile on macOS, and a
/// feature would be one more thing that can be forgotten in the argv while the
/// stage gates on a binary from some previous run.
#[must_use]
pub fn objc_class_audit_build_args() -> Vec<String> {
    [
        "--unverified",
        "build",
        "-q",
        "-p",
        "aterm-gui",
        "--example",
        OBJC_CLASS_AUDIT_EXAMPLE,
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// How the ladder reads the auditor's exit code (`0` clean / `1` finding / `2`
/// NOT RUN, declared in `crates/aterm-gui/examples/objc_live_class_audit.rs`).
///
/// A FUNCTION, and tested, for the same reason [`redraw_outcome`] is: `2` means
/// no window server answered, no delegate was installed, or the audit ran and
/// some row had no authority on this host at all; read as green it would restore
/// exactly the silence this gate exists to remove — the two plants it was
/// built against both left a GREEN build behind them.
///
/// `3` IS A PASS, AND IT IS A DIFFERENT CLAIM FROM `0` (2026-09-21). A protocol
/// the class claims may be one this host's AppKit does not register — macOS
/// 14.4.1's and macOS 13.7.8's lack `NSApplicationDelegate`, macOS 26's has it —
/// and `aterm_objc` then supplies a name-only stand-in so the class can claim it,
/// which is what stopped v0.72.0–v0.75.0 dying before their first window. A
/// stand-in declares nothing, so those rows once had NO authority and this
/// function read the whole audit as NOT RUN: the merge gate could not go green on
/// any such host, which made a whole platform unable to land anything. The
/// auditor now carries the fork's own declared shape for exactly those rows
/// (`HostLacks`), checks them against it, holds that written shape to the
/// protocol on every host that HAS one, and answers `3` to say which claim it
/// made. `0` still means what its label says — the runtime arbitrated every row —
/// and the two are not merged, because the receipt a green run writes would
/// otherwise assert runtime authority nobody had.
#[must_use]
pub fn objc_audit_outcome(code: Option<i32>) -> (Outcome, String) {
    match code {
        Some(0) => (
            Outcome::Ok,
            "objc live-class audit: every method the registered WinitWindowDelegate holds agrees with the runtime's own authority, and the class claims the protocols its rows come from".to_string(),
        ),
        Some(1) => (
            Outcome::Fail(Severity::GateFailed),
            "objc live-class audit: a registered encoding disagrees with its authority, a row has no authority at all, or the class does not claim a protocol it implements".to_string(),
        ),
        Some(2) => (
            Outcome::Fail(Severity::CouldNotRun),
            "objc live-class audit: NOT RUN — no event loop, no delegate was installed, or a row whose claimed protocol this host's AppKit does not register and for which the fork wrote down no shape either (aterm's name-only stand-in declares nothing; the auditor's NOT CHECKED lines name them), so the registered class was not fully proven (exit 2, never a pass)".to_string(),
        ),
        Some(3) => (
            Outcome::Ok,
            "objc live-class audit: every registered row agrees — and at least one was checked against THIS FORK'S OWN DECLARATION, not this host's runtime, because its AppKit does not register the protocol that declares it (the auditor's part F and VERDICT lines name each one). A host that registers the protocol holds those same written shapes to it on every run".to_string(),
        ),
        Some(c) => (
            Outcome::Fail(Severity::GateFailed),
            format!("objc live-class audit: unexpected exit {c} (the auditor answers only 0/1/2/3)"),
        ),
        None => (
            Outcome::Fail(Severity::CouldNotRun),
            "objc live-class audit: no exit status — killed by a signal, or never spawned".to_string(),
        ),
    }
}

/// The toolbar driver's target name — the `[[example]]`, the built file and
/// the argv below all have to agree, so they read it from here.
pub const OBJC_TOOLBAR_DRIVE_EXAMPLE: &str = "objc_toolbar_drive";

/// `targo --unverified build -q -p aterm-gui --example objc_toolbar_drive`
///
/// NO `required-features`, for the same reason the auditor has none: the
/// driver needs nothing `aterm-gui` does not already compile on macOS, and a
/// feature is one more thing that can be forgotten in the argv while the stage
/// gates on a binary from some previous run.
#[must_use]
pub fn objc_toolbar_drive_build_args() -> Vec<String> {
    [
        "--unverified",
        "build",
        "-q",
        "-p",
        "aterm-gui",
        "--example",
        OBJC_TOOLBAR_DRIVE_EXAMPLE,
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// The window driver's target name — the `[[example]]`, the built file and the
/// argv below all have to agree, so they read it from here.
pub const OBJC_WINDOW_DRIVE_EXAMPLE: &str = "objc_window_drive";

/// `targo --unverified build -q -p aterm-gui --example objc_window_drive`
#[must_use]
pub fn objc_window_drive_build_args() -> Vec<String> {
    [
        "--unverified",
        "build",
        "-q",
        "-p",
        "aterm-gui",
        "--example",
        OBJC_WINDOW_DRIVE_EXAMPLE,
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// How the ladder reads the window driver's exit code (`0` clean / `1` finding
/// / `2` NOT RUN, declared in `crates/aterm-gui/examples/objc_window_drive.rs`).
///
/// THREE codes, not the toolbar's four: this driver enters no `-mouseDown:` IMP
/// and pops no menu, so there is no modal tracking loop to hang in and no
/// watchdog to report. It drives through winit's public API plus two rows —
/// `draggingEntered:` and `windowShouldClose:` — that no public API can reach.
#[must_use]
pub fn objc_window_outcome(code: Option<i32>) -> (Outcome, String) {
    match code {
        Some(0) => (
            Outcome::Ok,
            "objc window drive: the real window answered every question — title round trips through NSString, the style mask against AppKit's own -styleMask, position and size round trips, min/max clamping, visibility, theme through +appearanceNamed:/-bestMatchFromAppearancesWithNames:, shadow and tabs, draggingEntered: answering NSDragOperationCopy as an NSUInteger, windowShouldClose: answering NO, and the fullscreen state machine".to_string(),
        ),
        Some(1) => (
            Outcome::Fail(Severity::GateFailed),
            "objc window drive: the driven window did not behave — a round trip disagreed, a style-mask bit landed in the wrong place, or a delegate row answered the wrong value".to_string(),
        ),
        Some(2) => (
            Outcome::Fail(Severity::CouldNotRun),
            "objc window drive: NOT RUN — no event loop, no window, or no delegate was installed, so nothing was proven about the window surface (exit 2, never a pass)".to_string(),
        ),
        Some(c) => (
            Outcome::Fail(Severity::GateFailed),
            format!("objc window drive: unexpected exit {c} (the driver answers only 0/1/2) — a signal here is the shape of the use-after-free its first run found"),
        ),
        None => (
            Outcome::Fail(Severity::CouldNotRun),
            "objc window drive: no exit status — killed by a signal, or never spawned".to_string(),
        ),
    }
}

/// How the ladder reads the toolbar driver's exit code (`0` clean / `1`
/// finding / `2` NOT RUN / `3` HUNG, declared in
/// `crates/aterm-gui/src/toolbar_drive.rs`).
///
/// FOUR codes and not three, and the fourth is not decoration. The driver
/// enters `-mouseDown:` / `-rightMouseDown:` IMPs directly, and a context menu
/// that actually popped would run `-[NSMenu popUpContextMenu:…]`'s modal
/// tracking loop and never return. Its watchdog kills the process at 180 s
/// with `3`, so a hang reaches the ladder as a NAMED could-not-run rather than
/// as a stage that decided nothing while looking busy.
#[must_use]
pub fn objc_toolbar_outcome(code: Option<i32>) -> (Outcome, String) {
    match code {
        Some(0) => (
            Outcome::Ok,
            "objc toolbar drive: the real tab strip answered every question — chips, clicks, a drag, both menu routes, a resize, the rename editor's commit and cancel exits, a 200-rebuild ownership ledger, 27 drawn states, and all four declared classes read off live objects against the runtime's own authority".to_string(),
        ),
        Some(1) => (
            Outcome::Fail(Severity::GateFailed),
            "objc toolbar drive: the driven strip did not behave, a drawing state stopped drawing, or a registered encoding disagrees with its authority".to_string(),
        ),
        Some(2) => (
            Outcome::Fail(Severity::CouldNotRun),
            "objc toolbar drive: NOT RUN — no event loop, no window, or no toolbar was installed, so nothing was proven about the tab strip (exit 2, never a pass)".to_string(),
        ),
        Some(3) => (
            Outcome::Fail(Severity::CouldNotRun),
            "objc toolbar drive: HUNG — the watchdog fired, so a modal tracking loop never returned and the drive proved nothing (exit 3, never a pass)".to_string(),
        ),
        Some(c) => (
            Outcome::Fail(Severity::GateFailed),
            format!("objc toolbar drive: unexpected exit {c} (the driver answers only 0/1/2/3)"),
        ),
        None => (
            Outcome::Fail(Severity::CouldNotRun),
            "objc toolbar drive: no exit status — killed by a signal, or never spawned".to_string(),
        ),
    }
}

/// The IME driver's target name — the `[[example]]`, the built file and the
/// argv below all have to agree, so they read it from here.
pub const OBJC_IME_DRIVE_EXAMPLE: &str = "objc_ime_drive";

/// `targo --unverified build -q -p aterm-gui --example objc_ime_drive`
#[must_use]
pub fn objc_ime_drive_build_args() -> Vec<String> {
    [
        "--unverified",
        "build",
        "-q",
        "-p",
        "aterm-gui",
        "--example",
        OBJC_IME_DRIVE_EXAMPLE,
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// How the ladder reads the IME driver's exit code (`0` clean / `1` finding /
/// `2` NOT RUN, declared in `crates/aterm-gui/examples/objc_ime_drive.rs`).
///
/// A FUNCTION, and tested, for the same reason [`objc_audit_outcome`] is: `2`
/// means no window server answered or the view had no input context, and read
/// as green it would restore exactly the silence these gates exist to remove.
#[must_use]
pub fn objc_ime_outcome(code: Option<i32>) -> (Outcome, String) {
    match code {
        Some(0) => (
            Outcome::Ok,
            "objc IME drive: a composition ran through the ported WinitView's NSTextInputClient rows — preedit, UTF-16 to UTF-8 cursor conversion, the attributed-string branch, the out-of-range clamp, commit, and the candidate-window rectangle".to_string(),
        ),
        Some(1) => (
            Outcome::Fail(Severity::GateFailed),
            "objc IME drive: a composition did not produce the events winit's API promises, or the candidate-window rectangle was wrong".to_string(),
        ),
        Some(2) => (
            Outcome::Fail(Severity::CouldNotRun),
            "objc IME drive: NOT RUN — no event loop, no view, or no input context, so nothing was proven about the IME path (exit 2, never a pass)".to_string(),
        ),
        Some(c) => (
            Outcome::Fail(Severity::GateFailed),
            format!("objc IME drive: unexpected exit {c} (the driver answers only 0/1/2)"),
        ),
        None => (
            Outcome::Fail(Severity::CouldNotRun),
            "objc IME drive: no exit status — killed by a signal, or never spawned".to_string(),
        ),
    }
}

/// The event driver's target name — the `[[example]]`, the built file and the
/// argv below all have to agree, so they read it from here.
pub const OBJC_EVENT_DRIVE_EXAMPLE: &str = "objc_event_drive";

/// `targo --unverified build -q -p aterm-gui --example objc_event_drive`
#[must_use]
pub fn objc_event_drive_build_args() -> Vec<String> {
    [
        "--unverified",
        "build",
        "-q",
        "-p",
        "aterm-gui",
        "--example",
        OBJC_EVENT_DRIVE_EXAMPLE,
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// How the ladder reads the event driver's exit (`0` clean / `1` finding /
/// `2` NOT RUN, declared in `crates/aterm-gui/examples/objc_event_drive.rs`).
///
/// ONE DELIBERATE DIFFERENCE from its three siblings: an ABORT — exit `3`
/// from the driver's own signal trap, or no exit status at all when the trap
/// could not convert it — is a GATE FAILURE here, not could-not-run. For
/// the other drivers a signal is an accident of the harness; for this one it
/// is the very finding it exists to make. v0.72.0 died by `SIGABRT` when a
/// `WinitView` row sent a key-only accessor to a mouse event inside a
/// `declare_class!` trampoline, and the driver reproduces exactly that shape
/// (measured: exit 134 at the first `mouseMoved:` against the v0.72.0
/// `view.rs`). Reading that as "decided nothing" would be reading the crash
/// as a shrug.
///
/// Since exception containment landed in `aterm_objc`, the driver's two
/// CONTROL children pin both halves of the trampoline's policy: the v0.72.0
/// send must now be CONTAINED (exit 0, the containment line on stderr naming
/// `poke:` and `NSInternalInconsistencyException`, a later send answering),
/// and a Rust panic must still ABORT with `abort_on_unwind`'s message. Either
/// control answering the other way is exit `1`.
#[must_use]
pub fn objc_event_outcome(code: Option<i32>) -> (Outcome, String) {
    match code {
        Some(0) => (
            Outcome::Ok,
            "objc event drive: every NSEvent-taking row of the ported WinitView, and the NSApplication sendEvent: override, survived a real NSEvent of every type AppKit can deliver to it — mouse, drag, tracking, scroll, gesture, key, cancelOperation: and the defined kinds — the exception control CONTAINED the v0.72.0 send (named on stderr, later send answered) and the panic control still aborted".to_string(),
        ),
        Some(1) => (
            Outcome::Fail(Severity::GateFailed),
            "objc event drive: a relation failed (an event did not produce the WindowEvent winit promises), a row went undriven, or a control answered the wrong way (the exception control was not contained and named, or the panic control did not abort)".to_string(),
        ),
        Some(2) => (
            Outcome::Fail(Severity::CouldNotRun),
            "objc event drive: NOT RUN — no event loop, no window, no view, or the control could not be spawned, so nothing was proven about the event rows (exit 2, never a pass)".to_string(),
        ),
        Some(3) => (
            Outcome::Fail(Severity::GateFailed),
            "objc event drive: ABORTED — a WinitView row raised an NSException or panicked on an NSEvent AppKit can deliver (the v0.72.0 mouse-move crash class); the driver's signal trap turned the abort into exit 3 and the transcript names the row".to_string(),
        ),
        Some(c) => (
            Outcome::Fail(Severity::GateFailed),
            format!("objc event drive: unexpected exit {c} (the driver answers only 0/1/2/3)"),
        ),
        None => (
            Outcome::Fail(Severity::GateFailed),
            "objc event drive: ABORTED — the driver died by a signal its trap could not convert, which for this driver is still the finding it exists to make: a WinitView row raised an NSException or panicked on an NSEvent AppKit can deliver (the v0.72.0 mouse-move crash class)".to_string(),
        ),
    }
}

/// The container driver's target name — the built file and the argv below
/// have to agree, so they read it from here. An `aterm-objc` example: the
/// capability under drive is that crate's own.
pub const OBJC_BOUND_DRIVE_EXAMPLE: &str = "objc_bound_drive";

/// `targo --unverified build -q -p aterm-objc --example objc_bound_drive`
#[must_use]
pub fn objc_bound_drive_build_args() -> Vec<String> {
    [
        "--unverified",
        "build",
        "-q",
        "-p",
        "aterm-objc",
        "--example",
        OBJC_BOUND_DRIVE_EXAMPLE,
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// How the ladder reads the container driver's exit code (`0` clean / `1`
/// finding / `2` NOT RUN, declared in
/// `crates/aterm-objc/examples/objc_bound_drive.rs`).
///
/// The driver's hang differential runs in CHILD processes under its own 5 s
/// watchdog and reports through `1`; the parent has no hang of its own to
/// name, so there is no fourth code.
#[must_use]
pub fn objc_bound_outcome(code: Option<i32>) -> (Outcome, String) {
    match code {
        Some(0) => (
            Outcome::Ok,
            "objc bound drive: MainThreadBound dropped its payload on the main thread from a worker — a plain T and a declared class's -dealloc with a Rust destructor — where the unsound twin declared beside it dropped both on the worker, and the needs_drop short-circuit was proved load-bearing by a child that hangs without it and returns with it".to_string(),
        ),
        Some(1) => (
            Outcome::Fail(Severity::GateFailed),
            "objc bound drive: a destructor landed on the wrong thread, the unsound twin stopped being unsound (the comparison lost its subject), or the hang differential answered the wrong way".to_string(),
        ),
        Some(2) => (
            Outcome::Fail(Severity::CouldNotRun),
            "objc bound drive: NOT RUN — no main thread or no child could be spawned, so nothing was proven about the container (exit 2, never a pass)".to_string(),
        ),
        Some(c) => (
            Outcome::Fail(Severity::GateFailed),
            format!("objc bound drive: unexpected exit {c} (the driver answers only 0/1/2) — a signal here is the shape of a -dealloc on the wrong thread"),
        ),
        None => (
            Outcome::Fail(Severity::CouldNotRun),
            "objc bound drive: no exit status — killed by a signal, or never spawned".to_string(),
        ),
    }
}

/// The nested workspace's checked-in driver is the oracle contract. Keeping
/// this as one argv value (with no cargo fallback or reimplementation here)
/// means changes to its cell table automatically reach the required merge
/// gate. The target dir is absolute because `run.sh`'s children do not share a
/// cwd — the native cell runs from the oracle workspace and the negative
/// controls from a copy of it under `$TMPDIR` — so a relative caller value
/// would resolve differently for each, while inheriting an arbitrary absolute
/// one would collapse this scheduler lane onto somebody else's Cargo lock.
///
/// # Errors
/// Returns the current-directory error from making a relative repository root
/// absolute. The stage reports that as COULD-NOT-RUN.
pub fn libc_oracle_cmd(ctx: &Ctx) -> std::io::Result<Cmd> {
    let root = std::path::absolute(&ctx.root)?;
    Ok(Cmd::new(root.join("libc-oracle/run.sh"))
        .env("CARGO_TARGET_DIR", root.join("libc-oracle/target"))
        // Python imports inside gen/ are attributed source unless bytecode is
        // suppressed. Set it here as a required-stage contract as well as in
        // run.sh, so a gate invocation cannot dirty its own fingerprint.
        .env("PYTHONDONTWRITEBYTECODE", "1"))
}

/// `libc-oracle/run.sh` exit contract: 0 proves conformance, 1 is a finding,
/// and 3 means a preflight/environment inability decided nothing — the
/// driver's own preflight, or `gen/symgate.py` answering 3 because the
/// reference libc could not be documented under Trust, which run.sh
/// propagates rather than folding into a finding about the shim. A missing
/// status likewise decided nothing; every other status violates the driver's
/// declared contract and remains a gate finding.
#[must_use]
pub fn libc_oracle_outcome(code: Option<i32>) -> Outcome {
    match code {
        Some(crate::exit::PASS) => Outcome::Ok,
        Some(crate::exit::COULD_NOT_RUN) | None => Outcome::Fail(Severity::CouldNotRun),
        Some(_) => Outcome::Fail(Severity::GateFailed),
    }
}

// ---------------------------------------------------------------------------
// Shared shapes
// ---------------------------------------------------------------------------

/// The script's `run()`: run, print what the child said, and decide.
///
/// Returns whether the child ran and passed.
fn run_labeled(ctx: &Ctx, r: &mut Report, label: &str, cmd: &Cmd) -> bool {
    run_child(ctx, r, label, cmd, |r, run, label| {
        r.decide_child(run, label)
    })
}

/// [`run_labeled`] for a CHECKER whose failing output can show it ran to its
/// end ([`Report::decide_checker_child`]): its finding is inherited from main
/// only when `ran_to_end` says it did. Every other child's never is
/// ([`crate::differential::UNFINISHED`]).
fn run_checker(
    ctx: &Ctx,
    r: &mut Report,
    label: &str,
    cmd: &Cmd,
    ran_to_end: crate::differential::RanToEnd,
) -> bool {
    let out = exec::run(cmd, ctx.exec_env());
    r.raw(out.output.as_str());
    r.decide_checker_child(&out, label, ran_to_end);
    out.ok
}

/// [`run_labeled`] for a child that RUNS tests (`targo test`): a failure is
/// itemized per failed test ([`Report::decide_test_child`]), which is what the
/// differential verdict compares with main's receipt
/// ([`crate::differential`]).
fn run_tests_labeled(ctx: &Ctx, r: &mut Report, label: &str, cmd: &Cmd) -> bool {
    run_child(ctx, r, label, cmd, |r, run, label| {
        r.decide_test_child(run, label);
    })
}

fn run_child(
    ctx: &Ctx,
    r: &mut Report,
    label: &str,
    cmd: &Cmd,
    decide: fn(&mut Report, &exec::Run, &str),
) -> bool {
    let out = exec::run(cmd, ctx.exec_env());
    r.raw(out.output.as_str());
    decide(r, &out, label);
    out.ok
}

/// The script's `run_scoped()`: [`run_checker`] for the stages that COMPILE
/// the selected crates.
///
/// An empty selection must not fall through to a bare `targo build` — with no
/// `-p` that builds the whole default workspace, which would be a full build
/// wearing a narrow run's label. Only `--changed` can select nothing (a
/// docs-only branch), and it is an honest skip, counted and named like any
/// other, so the verdict says the run compiled nothing.
fn run_scoped(
    ctx: &Ctx,
    r: &mut Report,
    label: &str,
    cmd: &Cmd,
    ran_to_end: crate::differential::RanToEnd,
) {
    if ctx.scope.selects_nothing() {
        r.skip(format!("{label} (change-scoped run selected no crates)"));
        return;
    }
    run_checker(ctx, r, label, cmd, ran_to_end);
}

/// A `targo` invocation, always naming its lane.
fn targo(ctx: &Ctx, args: Vec<String>) -> Cmd {
    Cmd::new(&ctx.tools.targo).args(args)
}

/// `CARGO_BUILD_JOBS` for a side lane, `None` where cargo's default stays.
///
/// Initial values (2026-09-13), to be tuned from the stage timings: the side
/// lanes overlap the build, and uncapped each would claim every core. Cargo does
/// not fingerprint the job count, so a cap never causes a rebuild.
///
/// `ConformanceRelease` joined on 2026-09-22 at 6 — an initial value like the
/// rest, above the four-job verb lanes because it is one long release compile
/// of the whole `-p aterm` graph rather than a handful of small ones, and below
/// the driver lane's eight because the driver lane's binaries gate the
/// EXCLUSIVE smokes at the tail while this one only has to beat the test
/// stage's first conformance suite. The row's `  time  ` line is what to
/// tune it from.
#[must_use]
pub const fn lane_build_jobs(lane: Lane) -> Option<u32> {
    match lane {
        Lane::XtaskTarget => Some(4),
        Lane::ConformanceRelease => Some(6),
        Lane::DriverTarget => Some(8),
        _ => None,
    }
}

/// The job cap a side lane's child gets: [`lane_build_jobs`], and never more
/// than the caller's own `CARGO_BUILD_JOBS` when that parses as a positive
/// integer. The constants were sized on a many-core builder; on a 4-core,
/// 8-thread Intel MacBook Pro the overlapping lanes (main and lint at cargo's
/// default 8, drivers 8, three side lanes 4 each) were measured at a load
/// average of 17–20 for the whole first run (2026-09-15), and a caller who
/// exported `CARGO_BUILD_JOBS=4` saw only the main and lint lanes obey it —
/// the side lanes overrode it back UP. A caller's value that does not parse is
/// left alone: it still reaches cargo by inheritance on the main lane and
/// fails there with cargo's own message, exactly as before.
#[must_use]
pub fn lane_jobs(lane: Lane, caller: Option<&std::ffi::OsStr>) -> Option<u32> {
    let cap = lane_build_jobs(lane)?;
    let ceiling = caller
        .and_then(|v| v.to_str())
        .and_then(|s| s.trim().parse::<u32>().ok())
        .filter(|&n| n > 0);
    Some(ceiling.map_or(cap, |c| cap.min(c)))
}

/// Put a cargo child in its lane: that lane's own `CARGO_TARGET_DIR` and job
/// cap, through the command's environment. `MainTarget` and `Pure` come back
/// unchanged — main-lane children inherit the caller's target dir exactly as
/// they always did.
#[must_use]
pub fn in_lane(ctx: &Ctx, lane: Lane, cmd: Cmd) -> Cmd {
    if matches!(lane, Lane::MainTarget | Lane::Pure) {
        return cmd;
    }
    let Some(dir) = lane_dir(ctx, lane) else {
        return cmd;
    };
    let cmd = cmd.env("CARGO_TARGET_DIR", dir);
    match lane_jobs(lane, ctx.env.cargo_build_jobs.as_deref()) {
        Some(jobs) => cmd.env("CARGO_BUILD_JOBS", jobs.to_string()),
        None => cmd,
    }
}

/// A build of a binary the gate DRIVES, in the driver lane. Only the build is
/// [`Cmd::demoted`]; the driven binary runs as its own child at the inherited
/// tier.
#[must_use]
pub fn driver_build_cmd(ctx: &Ctx, args: Vec<String>) -> Cmd {
    in_lane(ctx, Lane::DriverTarget, targo(ctx, args)).demoted()
}

/// The driver lane's target dir: every driven binary is resolved under it.
#[must_use]
pub fn drivers_dir(ctx: &Ctx) -> std::path::PathBuf {
    lane_dir(ctx, Lane::DriverTarget).unwrap_or_else(|| ctx.root.join("target-drivers"))
}

/// A binary the driver lane built: `<target-drivers>/debug/<name>`. The
/// ordinary debug binaries ARE the driven artifacts; launching them directly
/// (rather than through `targo run`) keeps the recorded PID attached to the
/// real process and the driver's lane banner out of a captured reply.
#[must_use]
pub fn driver_bin(ctx: &Ctx, name: &str) -> std::path::PathBuf {
    drivers_dir(ctx).join("debug").join(name)
}

/// An `[[example]]` the driver lane built — cargo puts those under
/// `<target>/debug/examples/`, not beside the binaries.
#[must_use]
pub fn driver_example(ctx: &Ctx, name: &str) -> std::path::PathBuf {
    driver_bin(ctx, "examples").join(name)
}

/// An xtask verb, in the xtask lane.
fn xtask_cmd(ctx: &Ctx, args: Vec<String>) -> Cmd {
    in_lane(ctx, Lane::XtaskTarget, targo(ctx, args))
}

/// The L0 gate's build, with its target dir named rather than inherited. It
/// is the directory cargo picked when nothing was exported
/// (`tools/freeze-safety-gate` is its own workspace); a caller's export used
/// to move it onto somebody else's lock.
fn freeze_gate_cmd(ctx: &Ctx) -> Cmd {
    in_lane(ctx, Lane::FreezeGateTarget, targo(ctx, freeze_gate_args()))
}

/// Bind Trust's renamed documentation driver for the stages that run doctests.
fn with_trustdoc(ctx: &Ctx, cmd: Cmd) -> Cmd {
    cmd.env("RUSTDOC", ctx.tools.trustdoc.as_os_str())
}

/// Which doc driver a doc-running stage gets. `Stage2` is the pinned driver,
/// bound through `RUSTDOC` (a `Command::env` that overrides anything the
/// caller exported — the gate pins its own driver whenever it has one).
/// `Ambient` is a caller-exported `RUSTDOC`/`CARGO_BUILD_RUSTDOC`: cargo
/// prefers either over the config's `[build] rustdoc`, so the child runs under
/// the operator's own binding and the ladder says so. `BarePath` leaves
/// cargo's discovery to resolve the config's bare `trustdoc` from the
/// children's PATH (the `~/.local/bin` farm link) — fail-closed, real doctest
/// verdicts. `Absent` means a doctest-compiling run would die at exec with a
/// raw OS error naming no remedy, so the stage diagnoses instead — unless the
/// run compiles no lib target (rustdoc is never spawned); the stage arms below
/// hold that qualifier.
#[derive(Debug, PartialEq, Eq)]
enum DocDriver {
    Stage2,
    Ambient,
    BarePath,
    Absent,
}

fn doc_driver(ctx: &Ctx) -> DocDriver {
    doc_driver_from(
        ctx.tools.have_trustdoc(),
        ctx.env.rustdoc_override.is_some(),
        crate::have_on_path("trustdoc", &ctx.path_env),
    )
}

/// Would this scope's `targo test` compile any doctests? Cargo spawns rustdoc
/// only for lib targets, so a scope with none runs green without any doc
/// driver — the Absent diagnosis must not fire there. `--changed` answers from
/// its resolved `Members` table; `--scope <crate>` never resolved the graph,
/// so it asks the member's own manifest (`crates/<name>` layout, both halves
/// of cargo's rule); the workspace always holds libs. Unanswerable answers
/// YES, the same fail-closed direction as `Members::any_has_lib` — here the
/// diagnosis rather than a bare run that would die raw if the answer was
/// really yes.
fn scope_compiles_doctests(ctx: &Ctx) -> bool {
    match &ctx.scope {
        Scope::Crate(c) => crate::changed::crate_dir_has_lib(&ctx.root, c),
        s => s.has_lib_target(),
    }
}

/// The rule as a truth table — pure, so it is a test and not a promise. The
/// stage2's own copy outranks everything: the gate runs THE toolchain's
/// drivers, and its `RUSTDOC` binding overrides even a caller's export. The
/// caller's export outranks the bare PATH walk because cargo gives it exactly
/// that precedence over the config key. The farm link is the fallback for
/// machines whose stage2 predates trustdoc, never a preference.
const fn doc_driver_from(
    stage2_has_trustdoc: bool,
    ambient_override: bool,
    bare_on_path: bool,
) -> DocDriver {
    if stage2_has_trustdoc {
        DocDriver::Stage2
    } else if ambient_override {
        DocDriver::Ambient
    } else if bare_on_path {
        DocDriver::BarePath
    } else {
        DocDriver::Absent
    }
}

/// A helper script under `tools/` (or anywhere), run as `<script> <root>`.
fn script_cmd(path: &std::path::Path, root: &std::path::Path) -> Cmd {
    Cmd::new(path).arg(root)
}

// ---------------------------------------------------------------------------
// 1) TEST COMPILE — the test stage's `--no-run` child, at t0 (2026-09-27), in
//    the slot a `targo build --workspace` row held until then. It is the
//    stage that says COULD NOT RUN when there is no pinned `targo`.
// 2) TEST — two children since 2026-09-13: compile every test target
//    (`--no-run`, a fingerprint check after the row above), then run them
//    (`--tests`), the second only if the first compiled. Doctests are not run
//    here: the doctests stage is their only runner, so this stage does not
//    diagnose a missing doc driver. It still binds trustdoc when the stage2
//    has one, so every child of the two rows carries one environment and the
//    compile above is the one the run's own `--no-run` finds.
//
//    Since 2026-09-26 the `--tests` child runs with a RECORDING runner, and
//    the gate runs the test binaries cargo recorded `--test-jobs` at a time,
//    the few that cannot share the machine alone ([`crate::testrun`]). The row
//    is decided from one cargo-shaped log in cargo's order, exactly as the
//    serial child's was — which still runs whenever the recording does not add
//    up, said in one line. The compile asks cargo for its JSON, which names
//    every test executable; both rows take the JSON out of what they print
//    ([`compile_tests`]).
// ---------------------------------------------------------------------------

/// The doc-driver binding the test children share, and the label suffix that
/// names it: the stage2's trustdoc when it has one, the caller's own export,
/// or nothing — no test child spawns rustdoc, so an absent driver is the
/// doctests stage's to diagnose.
fn test_doc_binding(ctx: &Ctx) -> (&'static str, bool) {
    match doc_driver(ctx) {
        DocDriver::Stage2 => (" (trustdoc)", true),
        DocDriver::Ambient => (" (caller's RUSTDOC)", false),
        DocDriver::BarePath | DocDriver::Absent => ("", false),
    }
}

fn test_compile(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        // Fail-closed, and COULD-NOT-RUN rather than FAILED: nothing about the
        // tree was decided. Never a stock-cargo fallback — that would make the
        // gate quietly unverified, which is what the two-lane driver prevents.
        r.cannot_run(ctx.tools.missing_targo_label());
        return;
    }
    let label = format!("targo test {} --no-run", ctx.scope.label());
    // The empty change-selection outranks the doc-driver binding, as in the
    // test stage below: a run that compiles NOTHING names no driver.
    if ctx.scope.selects_nothing() {
        r.skip(format!("{label} (change-scoped run selected no crates)"));
        return;
    }
    let (suffix, bind) = test_doc_binding(ctx);
    let [compile, _] = test_cmds(ctx, bind);
    // The row decides on the compile; the executables its JSON names are the
    // test stage's to read, from its own `--no-run` over this compile.
    let _ = compile_tests(ctx, r, &format!("{label}{suffix}"), &compile);
}

/// Run the test compile (`--message-format=json-render-diagnostics`) and
/// decide `label` on what it printed with cargo's JSON taken out — the log as
/// it read before the JSON was asked for — and return whether it compiled and
/// the test executables its JSON reported ([`crate::testrun::take_test_executables`]),
/// which the test run's recording is checked against.
fn compile_tests(
    ctx: &Ctx,
    r: &mut Report,
    label: &str,
    compile: &Cmd,
) -> (bool, Result<Vec<crate::testrun::TestExe>, String>) {
    let mut out = exec::run(compile, ctx.exec_env());
    let exes = crate::testrun::take_test_executables(&mut out.output);
    r.raw(out.output.as_str());
    r.decide_child(&out, label);
    (out.ok, exes)
}

/// The test stage's two children, compile then run, with trustdoc bound when
/// `bind`. Only the COMPILE is [`Cmd::demoted`]. The run skips the paint and
/// spin guards ([`MEASURING_TESTS`]; the measuring stage runs them), but it
/// still runs code, and a QoS clamp would reach whatever it launches. The run
/// carries [`TRAIL_LAWS_FULL`].
fn test_cmds(ctx: &Ctx, bind: bool) -> [Cmd; 2] {
    let cmd = |args: Vec<String>| {
        let c = targo(ctx, args);
        if bind { with_trustdoc(ctx, c) } else { c }
    };
    let (k, v) = TRAIL_LAWS_FULL;
    [
        cmd(test_compile_args(&ctx.scope)).demoted(),
        cmd(test_run_args(&ctx.scope)).env(k, v),
    ]
}

/// What every suite a failed build kept from running adds to its COULD NOT
/// RUN row (2026-09-27, third review).
///
/// WHY IT IS A ROW AT ALL. Each of these was a raw `not run:` line, invisible
/// to the tally: the build's FAIL above it was "the decision". Under the
/// absolute rule it was — but a build red main already has was INHERITED under
/// the differential, and then nothing at all stood for the suite behind it: a
/// branch could break every test the compile kept from running, and claim the
/// merge contract. A red main has is excused; what it kept from being decided
/// is not.
///
/// EVERY BUILD THAT FEEDS A DRIVE (2026-09-27, fourth review). The third
/// review's rows covered four sites; the redraw harness, the eight objc
/// drivers, the window-server rows and the control-socket smoke still ended
/// at the build's FAIL, and a rotted example on main let a branch break what
/// its driver checks and claim the contract (the review's P1, P3). Each now
/// goes through [`build_to_drive`] or says the same (the window-server rows
/// and two of those drivers, alert and swizzle, were deleted the same day in
/// gate decruft wave 2; the six left are [`OBJC_DRIVER_EXAMPLES`]). (A build's own row is
/// never inherited either since that day — nothing shows a build reached the
/// crates after its first error, [`crate::differential::UNFINISHED`] — so
/// this row is what names what the build kept from running.)
pub const NOT_RUN_BEHIND: &str = "; nothing behind it was decided, and a red main's receipt \
     excuses never excuses what it kept from running";

/// The COULD NOT RUN row for `what`, which the failure above it kept from
/// running ([`NOT_RUN_BEHIND`]).
pub fn not_run_behind(r: &mut Report, what: &str, because: &str) {
    r.cannot_run(format!("{what} — not run: {because}{NOT_RUN_BEHIND}"));
}

/// Build what a stage DRIVES, in the driver lane: `true` when it built. When
/// it did not, the build's own row — and, behind it, a COULD NOT RUN row for
/// `drive`, the check it kept from running ([`not_run_behind`]); only one row
/// when the machine refused the build, which is COULD NOT RUN already.
fn build_to_drive(
    ctx: &Ctx,
    r: &mut Report,
    args: Vec<String>,
    build_label: String,
    drive: &str,
) -> bool {
    let build = exec::run(&driver_build_cmd(ctx, args), ctx.exec_env());
    if build.ok {
        return true;
    }
    r.raw(build.output.as_str());
    r.fail_child(&build, build_label);
    if build.environment_failure().is_none() {
        not_run_behind(
            r,
            drive,
            "the build above failed, so there is nothing fresh to drive",
        );
    }
    false
}

/// **THE RAINBOW TRAIL LAWS RUN THEIR FULL GRID IN THE GATE** (2026-09-25).
/// `aterm-effects`' case-family laws (`tests/trail_host/mod.rs`) run each
/// family's SPINE in a debug build — the cases either side of every stated
/// threshold — and every case of the family's grid in a release build or
/// with this variable set. Their LIMIT rules are written against the full
/// grid (`holds_but`: every case a rule names must read bad and every other
/// must hold), so the test run sets it and the gate enforces the rules
/// whole, not only a release run by hand. Read at run time, so the compile
/// child needs no copy of it and nothing is rebuilt.
pub const TRAIL_LAWS_FULL: (&str, &str) = ("TRAIL_LAWS_FULL", "1");

fn test(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        r.skip("targo test (no targo)");
        return;
    }
    let label = format!("targo test {}", ctx.scope.label());
    // The empty change-selection outranks the doc-driver verdict: a run that
    // compiles NOTHING needs no doc driver, and must keep saying why it ran
    // nothing rather than blaming a tool it never needed.
    if ctx.scope.selects_nothing() {
        r.skip(format!("{label} (change-scoped run selected no crates)"));
        return;
    }
    let (suffix, bind) = test_doc_binding(ctx);
    let [compile, run] = test_cmds(ctx, bind);
    let run_label = format!("{label} --tests{suffix}");
    let compile_label = format!("{label} --no-run{suffix}");
    let (compiled, exes) = compile_tests(ctx, r, &compile_label, &compile);
    if compiled {
        crate::testrun::run_tests(ctx, r, &run_label, &run, exes);
    } else {
        // Not a skip: nothing was absent, and the single child it replaced ran
        // no test after a compile error either. But not SILENT (2026-09-27,
        // third review): the compile's FAIL is a finding main's receipt can
        // excuse, and an excused red must not excuse the tests it kept from
        // running — so the run it prevented decided nothing, and says so.
        r.cannot_run(format!(
            "{run_label} — not run: the compile above failed, and cargo runs no test after a compile error{}",
            NOT_RUN_BEHIND
        ));
    }
}

// ---------------------------------------------------------------------------
// 2.2) THE TESTS THE RUN ABOVE SKIPPED, each list run EXCLUSIVELY: with nothing
//    else in flight, as the smokes are, because their verdicts depend on how
//    busy the machine is. The test run's own package selection, narrowed to
//    the targets that hold the list and filtered to it; never demoted, because
//    the paint and spin rows launch the aterm they judge and the deadline tests
//    time real launchd work. Each stage is planned only when its crate is
//    selected (`plan.rs`).
//      * DEADLINE TESTS ([`DEADLINE_TESTS`]) — LAND tier, the merge contract.
//      * MEASURING TESTS ([`MEASURING_TESTS`]) — MEASURE tier. Its first child
//        is the release build ([`conformance_release_cmd`], a fingerprint check
//        after the prime row) and the suites run only if it succeeded, handed
//        that artifact — never one a previous run left in the lane.
// ---------------------------------------------------------------------------

/// The paint and spin suites' prebuilt-artifact overrides. Keep each name at
/// its child-environment writer so the source guard can distinguish this
/// handoff from an ambient read. The gate sets both to the same artifact.
const RELEASE_PAINT_BIN: &str = "ATERM_PAINT_BIN";
const RELEASE_SPIN_BIN: &str = "ATERM_SPIN_BIN";

/// The release `aterm` [`conformance_release_cmd`] leaves: `<lane>/release/aterm`.
#[must_use]
pub fn conformance_release_binary(ctx: &Ctx) -> std::path::PathBuf {
    lane_dir(ctx, Lane::ConformanceRelease)
        .unwrap_or_else(|| ctx.root.join("target/conformance-release"))
        .join("release/aterm")
}

/// A filtered stage's test child — [`filtered_test_args`] over the run's scope,
/// with trustdoc bound the way the test run binds it so both see one
/// environment — and its ladder label.
fn filtered_test_cmd(ctx: &Ctx, targets: &[&str], filters: &[&str], bind: bool) -> (String, Cmd) {
    let label = format!(
        "targo test {} {}{} -- {}",
        ctx.scope.label(),
        targets.join(" "),
        test_doc_binding(ctx).0,
        filters.join(" ")
    );
    let c = targo(ctx, filtered_test_args(&ctx.scope, targets, filters));
    (label, if bind { with_trustdoc(ctx, c) } else { c })
}

/// The measuring stage's test child: [`measuring_args`], handed the release
/// artifact through both child-environment overrides.
fn measuring_cmd(ctx: &Ctx, bind: bool) -> (String, Cmd) {
    let (label, cmd) = filtered_test_cmd(ctx, &MEASURING_TARGETS, &MEASURING_TESTS, bind);
    let bin = conformance_release_binary(ctx);
    let cmd = cmd
        .env(RELEASE_PAINT_BIN, bin.as_os_str())
        .env(RELEASE_SPIN_BIN, bin.as_os_str());
    (label, cmd)
}

/// The deadline stage's child: [`deadline_args`].
fn deadline_cmd(ctx: &Ctx, bind: bool) -> (String, Cmd) {
    filtered_test_cmd(ctx, &DEADLINE_TARGETS, &DEADLINE_TESTS, bind)
}

fn deadline_tests(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        r.skip("deadline tests (no targo)");
        return;
    }
    let (label, cmd) = deadline_cmd(ctx, test_doc_binding(ctx).1);
    run_tests_labeled(ctx, r, &label, &cmd);
}

fn measuring_tests(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        r.skip("measuring tests (no targo)");
        return;
    }
    let (label, cmd) = measuring_cmd(ctx, test_doc_binding(ctx).1);
    if run_labeled(
        ctx,
        r,
        CONFORMANCE_RELEASE_LABEL,
        &conformance_release_cmd(ctx),
    ) {
        run_tests_labeled(ctx, r, &label, &cmd);
    } else {
        // Not a skip: the build's FAIL above is the decision. Running the
        // suites anyway would judge whatever release artifact a previous run
        // left in the lane.
        r.raw(format!(
            "  not run: {label} — the release build above failed, so paint and spin have no fresh artifact to judge"
        ));
    }
}

// ---------------------------------------------------------------------------
// 2.5) DOCTESTS
// ---------------------------------------------------------------------------
fn doctests(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        r.skip("targo test --doc (no targo)");
        return;
    }
    let label = format!("targo test --doc {}", ctx.scope.label());
    // The empty change-selection outranks every other verdict, same as the test
    // stage directly above.
    if ctx.scope.selects_nothing() {
        r.skip(format!("{label} (change-scoped run selected no crates)"));
        return;
    }
    // `cargo test --doc -p …` is a hard ERROR — "no library targets found in
    // package `X`" — when NONE of the selected packages has a lib, and an
    // all-binary selection is an ordinary outcome under `--changed` (`xtask` is
    // bin-only, so a branch that edits only crates/xtask selects exactly
    // {xtask}) and under `--scope xtask` alike: this stage would go RED on a
    // completely healthy tree. A tier that cries wolf is worse than no tier,
    // so ask first — `scope_compiles_doctests`, the same answer the test
    // stage's diagnosis consults — and skip honestly: a skip is counted, and
    // the verdict narrows accordingly.
    if !scope_compiles_doctests(ctx) {
        r.skip(format!(
            "targo test --doc (no package in scope has a library target: {})",
            ctx.scope.label()
        ));
        return;
    }
    let cmd = targo(ctx, doctest_args(&ctx.scope));
    match doc_driver(ctx) {
        DocDriver::Stage2 => {
            run_tests_labeled(
                ctx,
                r,
                &format!("{label} (trustdoc)"),
                &with_trustdoc(ctx, cmd),
            );
        }
        DocDriver::Ambient => {
            run_tests_labeled(ctx, r, &format!("{label} (caller's RUSTDOC)"), &cmd);
        }
        DocDriver::BarePath => {
            run_tests_labeled(ctx, r, &label, &cmd);
        }
        // THE ONLY DOCTEST RUNNER since 2026-09-13, so the diagnosis lives
        // here: no doc driver means no doctest was decided, which is
        // COULD-NOT-RUN with the remedy — never a skip pointing elsewhere.
        DocDriver::Absent => r.cannot_run(ctx.tools.missing_trustdoc_label()),
    }
}

/// The sealed rung's two children, labelled, in the order they run — the
/// exact commands, so a test asserts on them rather than on a replica. Both are
/// in the DRIVER lane, and they name ONE target dir.
///
/// `two_nodes_sealed` boots real `aterm-gui`s that aterm-link cannot declare, so
/// its harness (`crates/aterm-link/tests/harness/mod.rs`, `built_binary`) FINDS
/// one: first in the target dir the test binary was built into, then
/// `$CARGO_TARGET_DIR`, then `<root>/target` — and refuses one older than any
/// input its cargo depfile names. The first child is what makes that first
/// search answer with a binary built from the sources under test: the smokes'
/// own build argv, so it is a fingerprint no-op after the driver builds row and
/// a real build when anything moved since. A suite compiled into any other dir
/// would search a directory this build never wrote.
///
/// Until 2026-09-14 the rung was the second child alone, at t0, in a
/// `target-sealed/` that never held an `aterm-gui`: the harness fell through to
/// `target/`'s, which the build stage was still relinking on a warm gate (5 of
/// 9 tests refused STALE) and which a cold gate had not built at all.
#[must_use]
pub fn sealed_lane_cmds(ctx: &Ctx) -> [(String, Cmd); 2] {
    [
        (
            "targo build -p aterm-gui -p aterm-ctl (the aterm-gui the sealed rung drives)"
                .to_string(),
            driver_build_cmd(ctx, smoke_build_args()),
        ),
        (
            "targo test -p aterm-link --features sealed --test two_nodes_sealed".to_string(),
            in_lane(ctx, Lane::DriverTarget, targo(ctx, sealed_lane_args())),
        ),
    ]
}

fn sealed_lane(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        r.skip("sealed fabric lane (no targo)");
        return;
    }
    let [(build_label, build), (run_label, run)] = sealed_lane_cmds(ctx);
    // An integration-test-only run compiles no doctests, so it has no
    // doc-driver rule to take: both children run as written.
    let built = run_labeled(ctx, r, &build_label, &build);
    if built {
        run_labeled(ctx, r, &run_label, &run);
    } else {
        // Not a skip: nothing was absent. Running the suite anyway would drive
        // a missing or STALE `aterm-gui` — nine red rows about a build, not
        // about the fabric — and the suite it keeps from running decided
        // nothing, which the tally hears ([`NOT_RUN_BEHIND`]).
        r.cannot_run(format!(
            "{run_label} — not run: the aterm-gui build above failed, so the rung has no fresh binary to drive{}",
            NOT_RUN_BEHIND
        ));
    }
}

// ---------------------------------------------------------------------------
// 2.7) TIPPY — the LINT gate runs under the TRUST toolchain's Clippy fork, built
//    from the Trust rustc fork, never stock clippy. Auto-detected and
//    fail-closed: present means `-D warnings` MUST pass; absent is an honest skip.
//    A SEPARATE target dir keeps the trust-toolchain artifacts off the stock build.
// ---------------------------------------------------------------------------
/// The exact command the lint stage spawns.
///
/// Extracted so the test can assert on THIS, not on a replica it built itself.
/// A test that re-spells `.env(..)` and then checks its own spelling passes even
/// when the stage stops setting the variable — and `CARGO_TARGET_DIR` is the one
/// that must not silently go missing: without it the Trust-toolchain lint
/// artifacts land in the stock build's target dir and the two lanes clobber
/// each other's caches.
fn tippy_cmd(ctx: &Ctx, bin: &std::path::Path, args: Vec<String>) -> Cmd {
    let dir = bin.parent().unwrap_or(&ctx.tools.stage2_dir).to_path_buf();
    let mut path = std::ffi::OsString::from(dir.as_os_str());
    path.push(":");
    path.push(&ctx.path_env);
    Cmd::new(bin)
        .args(args)
        .env("PATH", path)
        .env("CARGO_TARGET_DIR", ctx.root.join("target-tippy"))
        .env("TRUST_NO_MIGRATE_WARN", "1")
        // A lint only compiles.
        .demoted()
}

fn tippy(ctx: &Ctx, r: &mut Report) {
    let Some(bin) = ctx.tools.tippy.clone() else {
        r.skip(ctx.tools.missing_tippy_label());
        return;
    };
    // A lint red main has is inherited only when it kept no unit from being
    // linted ([`crate::differential::lint_reached_every_unit`]): one in a
    // library leaves every crate built on it unlinted.
    run_scoped(
        ctx,
        r,
        &format!("tippy {} -D warnings", ctx.scope.label()),
        &tippy_cmd(ctx, &bin, tippy_args(&ctx.scope)),
        crate::differential::lint_reached_every_unit,
    );
    // THE SECOND PASS, `--full` ONLY (2026-09-27; every tier until then).
    // `--all-targets` above built no target whose `required-features` are
    // off, so the six in [`GATED_LINT_FEATURES`] are linted here or nowhere —
    // a broken bench build survived four days that way. It costs a re-lint of
    // two packages at a wider feature set (139 unit variants the first pass
    // never builds), and the targets it reaches are benches and a harness bin,
    // not the shipped build. Its own row, so the ladder shows whether it ran.
    if ctx.mode == crate::Mode::Full
        && let Some(args) = tippy_gated_args(&ctx.scope)
    {
        run_scoped(
            ctx,
            r,
            "tippy required-features targets -D warnings",
            &tippy_cmd(ctx, &bin, args),
            crate::differential::lint_reached_every_unit,
        );
    }
}

/// FORMATTING — `xtask gate lint --fmt-only`, i.e. the formatter lane's BOTH
/// passes and no other lane.
///
/// This stage did not exist until 2026-08-31, and the gap was declared rather
/// than hidden: `verify.sh` ran a tippy stage and no fmt stage, and said so.
/// Declaring a limit is not covering it. `.githooks/pre-push` was advisory from
/// 2026-08-24, so between those two facts NOTHING in this repository ran
/// the formatter unless a human chose to — and the MEASURED consequence was
/// three consecutive rebases of `main` arriving with drift (5 files, 2, 1), one
/// of them in `aterm-link`, a crate outside `members = ["crates/*"]` that
/// `targo-fmt --all` structurally cannot see.
///
/// It is cheap enough to be uncontroversial: the check needs no compiler —
/// trustfmt parses and prints, it does not build — and cost 7.5 s over 1,761
/// tracked files on two measured runs. The `XtaskTarget` lane is for the xtask
/// binary this shells through, not for the check.
///
/// NO SKIP WITHOUT A TOOLCHAIN, and that asymmetry is deliberate: the lane
/// itself already distinguishes a formatter that found drift (FINDING) from one
/// that could not run (NOT RUN), and both block there. Adding a second opinion
/// here would let a stage-level skip hide a lane-level not-run — the exact
/// confusion the fmt lane spent a month in. The only skip is the one every
/// xtask stage shares: no targo, so the verb cannot be built at all.
fn formatting(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        r.skip("formatting (no targo)");
        return;
    }
    // Inherited only when the verb printed its verdict with every pass read
    // ([`crate::differential::fmt_ran_to_end`]).
    run_checker(
        ctx,
        r,
        "gate lint --fmt-only",
        &xtask_cmd(ctx, xtask_gate_args_with("lint", &["--fmt-only"])),
        crate::differential::fmt_ran_to_end,
    );
}

// ---------------------------------------------------------------------------
// 3) GREP GUARDS AND LICENSE HEADERS (zero-tolerance, always whole-tree): two
//    script children, each a decision of its own and each run whatever the
//    other decided — `license_check.sh` holds every `.rs` to its two-line SPDX
//    header. A missing script is a cannot-run, never a skip. (The headers were
//    a stage of their own until 2026-09-27.)
// ---------------------------------------------------------------------------

/// The scripts the grep-guards stage runs, in order.
pub const GUARD_SCRIPTS: [&str; 2] = ["grep_guard.sh", "license_check.sh"];

fn grep_guards(ctx: &Ctx, r: &mut Report) {
    for name in GUARD_SCRIPTS {
        let script = ctx.tools_dir().join(name);
        if !is_executable_file(&script) {
            r.cannot_run(format!(
                "{name} missing or not executable ({})",
                script.display()
            ));
            continue;
        }
        let out = exec::run(&script_cmd(&script, &ctx.root), ctx.exec_env());
        r.raw(out.output.as_str());
        r.decide_checker_child(&out, name, guard_ran_to_end(name));
    }
}

/// How a guard script's output shows it ran every check: its closing verdict
/// line ([`crate::differential::guard_ran_to_end`],
/// [`crate::differential::license_ran_to_end`]). A script this does not know
/// never shows it.
fn guard_ran_to_end(name: &str) -> crate::differential::RanToEnd {
    match name {
        "grep_guard.sh" => crate::differential::guard_ran_to_end,
        "license_check.sh" => crate::differential::license_ran_to_end,
        _ => |_| Err(crate::differential::UNFINISHED),
    }
}

// ---------------------------------------------------------------------------
// 3.5) DELIVERY TOOLING — the offline shell suites over the scripts that take
//    the app and its toolchain packages from a checkout to someone's machine:
//    a local dev bundle, the installer, a cut, the site, the atpkg index. Each
//    is a decision of its own, each runs on stubs of its own making, and a
//    missing suite is a cannot-run, never a skip. ("Release tooling" until
//    2026-09-26: a dev build is not a release.)
// ---------------------------------------------------------------------------

/// The delivery-tooling suites, in the order the stage runs them. Every one is
/// offline — the network never gets a packet — and self-contained under its own
/// mktemp dir (its header says how); all but one take seconds.
///
/// * `test-install-channel.sh` keeps tools/install.sh aligned with the in-app
///   updater's head pointer, tag grammar and exact asset identity, and pins the
///   Linux lane's head-only choice of target.
/// * `test-publish-export.sh` runs publish/manifest.txt and
///   publish/transforms.sh the way the publication engine does. It is the only
///   check that the export's rewrites stay narrow: dev never applies them, so a
///   widened `/Users/` rewrite would pass every Rust test here while the PUBLIC
///   source's `relocate.rs` machine-local prefix read `"/Users//"`. It also
///   holds config.sh's version clause to the `aterm MAJOR.MINOR.0` shape.
/// * `test-release-preflight.sh` drives tools/release-preflight.sh against a
///   fixture checkout and a stub engine.
/// * `test-after-cut.sh` drives publish/after-cut, the step that ends a release,
///   over the real site-sync.py download election: a release that fails the
///   shape check never reaches the site. It joined on 2026-09-24, after going
///   red unseen: the election began requiring a complete app inventory (DMG,
///   mac.zip and the signed appcast pair), the suite's DMG-only fixtures stopped
///   electing anything, and with no gate running it nothing said so.
///
/// FIVE MORE JOINED ON 2026-09-26, every one of them wired into nothing until
/// then (measured on 8c644d49e: no stage, xtask verb, hook or Rust test named
/// them). Each passed when run by hand that day — one only after 391420169
/// repaired it for macOS 27, which is the point: it had gone red with nothing
/// to say so.
///
/// * `test-install-guard.sh` — install.sh's whole-file execution guard (a
///   truncated `curl … | bash` runs nothing) and its `--dry-run` zero-mutation
///   contract, under stubbed `curl`/`gh` and a scratch HOME.
/// * `test-cargo-pin.sh` — the cargo gate `tools/dev-app.sh` and install.sh's
///   source-build lane share: a build runs only under the cargo that honours
///   rust-toolchain.toml (the atpkg store's `targo`, else rustup's proxy), with
///   every cargo, rustup and store a fake that refuses to build.
/// * `test-check-release-shape.sh` — tools/check-release-shape.sh, the
///   cutter-independent look at a published release's asset set, over inline
///   release JSON (the LEAN shape and public v0.63.0's FAT one).
/// * `test-site-sync.sh` — publish/post-promote, the hook that brings the site
///   up to date after a promote: a local bare repo cloned over `file://`, and
///   stub `gh`, `curl`, `firebase` and site scripts. The slow one (about a
///   minute); it overlaps the build like the rest of this `Lane::Pure` row.
/// * `test-dev-sign-id.sh` — tools/dev-sign-id.sh, through its `--keychain`
///   seam: a scratch keychain FILE under ~/Library/Keychains (the only location
///   that exercises the partition-list step the script exists for), never put
///   on the search list, deleted on exit; measured dialog-free. A PASS of 0
///   checks off macOS, where the script has nothing to do.
///
/// AND ONE DEV TOOL'S OWN CONTRACT, joined 2026-09-27 for the same reason:
///
/// * `test-agent-fabric-demo.py` — tools/agent-fabric-demo.py keeps its
///   runtime root (the broker, bridge and aterm logs) when a run FAILS, and
///   removes it when one passes. It drives the demo's `close` alone: no aterm,
///   no aterm-link, no broker, a second. Not delivery tooling — the demo drives
///   an installed aterm — but offline and self-contained like the rest, and a
///   row of its own for one Python check would cost more than it says.
///
/// AND THE ATPKG PUBLISH TOOLING'S TEN, joined 2026-09-27 from their own
/// driver-lane row, which they sat in beside the one suite that drives a
/// real `atpkg` (the end-to-end pack, [`ATPKG_DRIVEN_SUITE`]). None of them
/// drives one: each lays stub `atpkg`/`atpkg-keys`/`gh` shims under its own
/// mktemp dir, and several deliberately run a producer script with `$ATPKG`
/// unset — so, like every suite here, they are handed nothing.
///
/// * `test-atpkg-vendor-tooling.sh` — the https-protocol publish rows, the
///   spec flags column, the mirror's release-asset reader, and the publish
///   lock (a kernel lock; its holder's bring-up waits up to 10 s).
/// * `test-atpkg-mirror-extras.sh` — tools/atpkg-mirror-public.sh, the only
///   writer of public atpkg releases: the no-extras gate, the download retry,
///   and a work list that is the UNION of `pin` and every `pin_by_target`
///   overlay (it walked `pin` alone, and every overlay build 404'd).
/// * `test-atpkg-prerelease-gate.sh` — every atpkg release create carries
///   `--prerelease` (atpkg-index-15 held /releases/latest for ~5.4 h).
/// * `test-atpkg-target-pins.sh` and `test-atpkg-index-target-pins.sh` — the
///   channel's pins are PER-TARGET, and the indexer renders `pin_by_target`
///   exactly as the client reads it.
/// * `test-atpkg-index-publish.sh` — the indexer's public publish: baseline+1,
///   packs first, the compare-and-swap, never-clobber.
/// * `test-atpkg-spec-catch-up.sh` — the rustc-group lane catches every row
///   outside its group up to the public baseline, never lower, its preflight
///   refuses a sibling or seal packing below the public pin (or at it,
///   without `FORCE_SAME_BUILD=1`), and step 2 stops when a sibling checkout
///   moved after the preflight judged it.
/// * `test-atpkg-auto-alab.sh` — the ALab lane builds only what was signed
///   (a SKIP off macOS).
/// * `test-linux-auto-atpkg.sh` — the Linux stager serves EVERY Linux triple
///   the client can ask for, from one builder.
/// * `test-atpkg-pack-toolchain.sh` — the packers resolve the store's
///   `targo`, and a sysroot bundle signs the version of what it packs.
///
/// (`test-atpkg-pack-arch-gate.sh` is run by aterm-release's `pack_arch_gate`
/// test.) A suite no gate runs is a test that passes forever.
pub const DELIVERY_SUITES: [&str; 20] = [
    "test-install-channel.sh",
    "test-publish-export.sh",
    "test-release-preflight.sh",
    "test-after-cut.sh",
    "test-install-guard.sh",
    "test-cargo-pin.sh",
    "test-check-release-shape.sh",
    "test-site-sync.sh",
    "test-dev-sign-id.sh",
    "test-agent-fabric-demo.py",
    "test-atpkg-vendor-tooling.sh",
    "test-atpkg-mirror-extras.sh",
    "test-atpkg-prerelease-gate.sh",
    "test-atpkg-target-pins.sh",
    "test-atpkg-index-target-pins.sh",
    "test-atpkg-index-publish.sh",
    "test-atpkg-spec-catch-up.sh",
    "test-atpkg-auto-alab.sh",
    "test-linux-auto-atpkg.sh",
    "test-atpkg-pack-toolchain.sh",
];

/// One delivery suite's command. With a `SIGTERM` grace, because each suite
/// cleans up in an EXIT trap that a `SIGKILL` would skip — for
/// `test-dev-sign-id.sh` that is a scratch keychain in the owner's real
/// `~/Library/Keychains` ([`Cmd::term_grace`] has the measurement).
#[must_use]
pub fn delivery_suite_cmd(ctx: &Ctx, name: &str) -> Cmd {
    Cmd::new(ctx.tools_dir().join(name)).term_grace(exec::TERM_GRACE)
}

fn delivery_tooling(ctx: &Ctx, r: &mut Report) {
    for name in DELIVERY_SUITES {
        let t = ctx.tools_dir().join(name);
        if is_executable_file(&t) {
            run_labeled(ctx, r, name, &delivery_suite_cmd(ctx, name));
        } else {
            r.cannot_run(format!(
                "{name} missing or not executable ({})",
                t.display()
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// 3.52) ATPKG END-TO-END PACK — `tools/test-atpkg-pack-one-compiler.sh`, the
//    sysroot-bundle pack's one-compiler contract (atpkg-pack-bundle.sh):
//    keyless and offline, everything under one mktemp dir — and its section D
//    PACKS with a real `atpkg`. Where that pack can run at all, a missing
//    binary is a gap in the run rather than a platform limit, so it fails. A
//    DRIVER-LANE STAGE (since 2026-09-16; the ladder order is `plan.rs`): its
//    first child builds that binary in this lane, the pack runs only if that
//    build succeeded, and $ATPKG points at what was built. A missing suite is
//    a cannot-run, never a skip.
// ---------------------------------------------------------------------------

/// The one atpkg suite that drives a real `atpkg` rather than stubs of its own
/// making: section D of the pack contract, the end-to-end pack.
pub const ATPKG_DRIVEN_SUITE: &str = "test-atpkg-pack-one-compiler.sh";

/// The label of this stage's first child — the build the suite below depends on.
pub const ATPKG_BUILD_LABEL: &str = "targo build -p atpkg (the atpkg the pack suite drives)";

/// The binary [`ATPKG_DRIVEN_SUITE`] is pointed at: this lane's own
/// `debug/atpkg`, which this stage's first child writes. Never
/// `<root>/target/debug/atpkg` — that one belongs to the workspace build, which
/// runs in another lane, may still be linking it, and under a `--scope`
/// narrowing never builds it at all.
#[must_use]
pub fn atpkg_driven_binary(ctx: &Ctx) -> std::path::PathBuf {
    driver_bin(ctx, "atpkg")
}

/// The build of that binary, in the driver lane. Compile-only, so demoted.
#[must_use]
pub fn atpkg_build_cmd(ctx: &Ctx) -> Cmd {
    driver_build_cmd(ctx, atpkg_build_args())
}

/// The pack suite's command, handed `$ATPKG` = the binary this stage just
/// built. That binding is the half of the fix that ordering cannot do: the
/// script's own resolution order ends in `<root>/target/{debug,release}/atpkg`,
/// so an ordered stage whose lane dir is empty would still silently drive a
/// previous run's binary rather than saying it had none.
#[must_use]
pub fn atpkg_pack_cmd(ctx: &Ctx) -> Cmd {
    delivery_suite_cmd(ctx, ATPKG_DRIVEN_SUITE).env("ATPKG", atpkg_driven_binary(ctx))
}

fn atpkg_tooling(ctx: &Ctx, r: &mut Report) {
    let name = ATPKG_DRIVEN_SUITE;
    let t = ctx.tools_dir().join(name);
    if !is_executable_file(&t) {
        r.cannot_run(format!(
            "{name} missing or not executable ({})",
            t.display()
        ));
        return;
    }
    if !ctx.tools.have_targo() {
        // Counted and named: with no toolchain nothing can build the binary,
        // and section D's coverage is then genuinely absent from the run.
        r.skip(format!(
            "{name} (no targo — nothing built the atpkg its end-to-end pack drives)"
        ));
        return;
    }
    if run_labeled(ctx, r, ATPKG_BUILD_LABEL, &atpkg_build_cmd(ctx)) {
        run_labeled(ctx, r, name, &atpkg_pack_cmd(ctx));
    } else {
        // Not a skip: nothing was absent. Running the pack anyway would drive
        // whatever `<root>/target` happened to hold, and the pack the failed
        // build keeps from running decided nothing ([`NOT_RUN_BEHIND`]).
        r.cannot_run(format!(
            "{name} — not run: the atpkg build above failed, so the end-to-end pack has no fresh binary to drive{}",
            NOT_RUN_BEHIND
        ));
    }
}

// ---------------------------------------------------------------------------
// 3.56) TRUST CONTRACT PROBE — two facts about the INSTALLED stage2 that no
//    cargo stage exercises while the workspace opt-out is on: the off-switch
//    must compile a contract without ICEing (2026-07-30: the first committed
//    `ensures` crashed trustc under the then-current off-switch, so contracts
//    could not land at all), and a `self`-field postcondition must PROVE (same
//    date: every such predicate was UNPARSEABLE → fail-closed Unknown — the
//    exact form every lifecycle property in aterm takes). Both held on stage2
//    51bf8a270 (2026-08-05); tools/trust-probes/self_field_ensures.rs is the
//    probe. Skips without the toolchain like every stage2 stage; a MISSING
//    script is cannot-run, because a probe that vanishes is how the ICE class
//    returns unheralded.
// ---------------------------------------------------------------------------
fn trust_contract_probe(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        r.skip("trust contract probe (no stage2 toolchain)");
        return;
    }
    let t = ctx.tools_dir().join("test-trust-contract-probe.sh");
    if is_executable_file(&t) {
        run_labeled(ctx, r, "test-trust-contract-probe.sh", &Cmd::new(&t));
    } else {
        r.cannot_run(format!(
            "test-trust-contract-probe.sh missing or not executable ({})",
            t.display()
        ));
    }
}

// ---------------------------------------------------------------------------
// 3.6) STARTUP COMPARISON SCHEDULER (`--full` only since 2026-09-27) — the
//    publishable-startup evidence path must fail closed on malformed samples,
//    mutable harness bytes, uncertain thermal state, identical-artifact
//    controls, and timed-out process descendants.
// ---------------------------------------------------------------------------
fn start_compare(ctx: &Ctx, r: &mut Report) {
    let t = ctx.tools_dir().join("perf-arena/test-start-compare.sh");
    if is_executable_file(&t) {
        run_labeled(ctx, r, "test-start-compare.sh", &Cmd::new(&t));
    } else {
        r.cannot_run(format!(
            "test-start-compare.sh missing or not executable ({})",
            t.display()
        ));
    }
}

// ---------------------------------------------------------------------------
// 4.5a) FIRST-PARTY LIBC ABI ORACLE, for THIS HOST'S native cell. `targo test`
//    compiles the cell's const/layout/type assertions and executes its
//    pointer-valued and C-macro checks, and the emitted-source gate closes
//    link-name aliases — all under the Trust sysroot, the only compiler the
//    driver names. The other cells are decided where THEY are native (the
//    `FLEET_HOST_TRIPLES` rule in xtask's gate.rs): a merge-gate run on the
//    Intel Mac decides x86_64-apple-darwin, one on m17-tower decides
//    x86_64-unknown-linux-gnu, and a cell no box hosts is decided by no gate,
//    which the driver's `### CELLS` report says row by row. Until 2026-09-20
//    the driver also type-checked five cross cells on stock toolchains through
//    rustup; the owner's direction ("avoid extra dependencies and especially
//    we are avoiding rustc because we have trust!") removed that path, so a
//    box with no rustup — the atpkg store shape — can pass this stage.
//
//    Missing/non-executable driver and exit 3 are COULD-NOT-RUN, never a skip;
//    exit 1 is a conformance finding. There is no stock-workspace fallback:
//    this separate workspace is what prevents `[patch.crates-io]` from
//    rewriting the reference libc edge to the shim under test.
// ---------------------------------------------------------------------------
fn libc_oracle(ctx: &Ctx, r: &mut Report) {
    let cmd = match libc_oracle_cmd(ctx) {
        Ok(cmd) => cmd,
        Err(error) => {
            r.cannot_run(format!(
                "cannot resolve the libc-oracle target dir: {error}"
            ));
            return;
        }
    };
    if !is_executable_file(&cmd.program) {
        r.cannot_run(format!(
            "libc-oracle/run.sh missing or not executable ({})",
            cmd.program.display()
        ));
        return;
    }
    let label = "libc-oracle/run.sh (this host's native ABI cell; the other cells are decided where they are native)";
    let out = exec::run(&cmd, ctx.exec_env());
    r.raw(out.output.as_str());
    // THE MACHINE BEFORE THE EXIT CODE. `libc_oracle_outcome` reads run.sh's
    // exit contract (3 is COULD NOT RUN, every other non-zero is a finding),
    // and that contract cannot see a full disk: run.sh's own `step` maps a
    // cargo child that died of `No space left on device` — exit 101, because
    // targo never answers 3 — onto rc=1, which arrives here as a FINDING about
    // the tree. This stage builds into `libc-oracle/target`, one of the lane
    // dirs the disk floor exists to bound, so that is the stage most likely to
    // meet ENOSPC first.
    if r.child_could_not_run(&out, label) {
        return;
    }
    r.record_child(&out, libc_oracle_outcome(out.code), label);
}

// ---------------------------------------------------------------------------
// 4.5b) L0 TEMPORAL-SAFETY GATE — ONE build of tools/freeze-safety-gate enforces
//    six obligations (temporal proof, main-loop census, lock-order census,
//    wasm-process census, scope-cardinality census, lazy-init reentrancy
//    census), any one FAILING the build with a counterexample-backed
//    diagnostic.
// ---------------------------------------------------------------------------
fn freeze_gate(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        r.skip("L0 temporal-safety gate (no targo)");
        return;
    }
    run_labeled(
        ctx,
        r,
        "freeze-safety-gate (6 obligations)",
        &freeze_gate_cmd(ctx),
    );
}

// ---------------------------------------------------------------------------
// 4.7) DRIVER BUILDS (2026-09-13) — the binaries the tail drives, compiled at
//    t0 in `target-drivers/`. Before this row they compiled one after another
//    inside the stages that drive them, and the smokes' builds inside the
//    exclusive section. Every driver stage still runs its own build argv (a
//    fingerprint no-op after this) and drives only what that build left, so no
//    stage takes its binary on this row's word. That includes, on macOS, the
//    one `aterm` binary the live lanes drive ([`live_aterm_build_args`]),
//    whose stages sit at the very end of the serial tail; off macOS both lanes
//    are named skips, so nothing builds it.
// ---------------------------------------------------------------------------

/// The objc driver examples with the package each lives in, in the order
/// their stages run.
pub const OBJC_DRIVER_EXAMPLES: [(&str, &str); 6] = [
    ("aterm-gui", OBJC_CLASS_AUDIT_EXAMPLE),
    ("aterm-gui", OBJC_IME_DRIVE_EXAMPLE),
    ("aterm-gui", OBJC_TOOLBAR_DRIVE_EXAMPLE),
    ("aterm-gui", OBJC_WINDOW_DRIVE_EXAMPLE),
    ("aterm-gui", OBJC_EVENT_DRIVE_EXAMPLE),
    ("aterm-objc", OBJC_BOUND_DRIVE_EXAMPLE),
];

/// `targo --unverified build -q -p aterm-gui -p aterm-objc --example <each>` —
/// every objc driver in one cargo invocation. Graph-measured on 07a76fca7
/// (`--unit-graph`, with the build-override pin, over the eight drivers of
/// the day): the combined graph equals the union of the single-example
/// graphs, symmetric difference 0 — so it compiles exactly what the stage
/// builds compile.
#[must_use]
pub fn objc_driver_prebuild_args() -> Vec<String> {
    let mut a: Vec<String> = ["--unverified", "build", "-q"]
        .into_iter()
        .map(String::from)
        .collect();
    for (pkg, _) in OBJC_DRIVER_EXAMPLES {
        if !a.iter().any(|x| x == pkg) {
            a.push("-p".to_string());
            a.push(pkg.to_string());
        }
    }
    for (_, example) in OBJC_DRIVER_EXAMPLES {
        a.push("--example".to_string());
        a.push(example.to_string());
    }
    a
}

/// The driver-builds row's children, labelled: on macOS, where they run, the
/// objc drivers FIRST; then the two smoke binaries, the one `aterm` binary
/// the live lanes drive (macOS) and the redraw harness.
#[must_use]
pub fn driver_build_cmds(ctx: &Ctx) -> Vec<(String, Cmd)> {
    let mac = cfg!(target_os = "macos");
    let mut v = Vec::new();
    if mac {
        v.push((
            "driver prebuild: targo build --example (the objc drivers)".to_string(),
            driver_build_cmd(ctx, objc_driver_prebuild_args()),
        ));
    }
    v.push((
        "driver prebuild: targo build -p aterm-gui -p aterm-ctl".to_string(),
        driver_build_cmd(ctx, smoke_build_args()),
    ));
    if mac {
        v.push((
            "driver prebuild: targo build -p aterm --bin aterm (the live lanes' aterm)".to_string(),
            live_aterm_build_cmd(ctx),
        ));
    }
    v.push((
        format!("driver prebuild: targo build --bin {REDRAW_CONFORMANCE_BIN}"),
        driver_build_cmd(ctx, redraw_conformance_build_args()),
    ));
    v
}

fn driver_builds(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        r.skip("driver builds (no targo)");
        return;
    }
    // Every child, even after a failure: they are independent builds, and the
    // stage that drives each one reports its own build either way.
    for (label, cmd) in driver_build_cmds(ctx) {
        run_labeled(ctx, r, &label, &cmd);
    }
}

// ---------------------------------------------------------------------------
// 5b) CONFORMANCE RELEASE ARTIFACT — the RELEASE `aterm` that paint and spin
//    JUDGE, a fat-LTO build of ~326 s cold. This row builds it at t0 in a lane
//    of its own; the exclusive measuring stage runs the SAME command as its
//    first child (a fingerprint check by then) and hands the suites the
//    artifact through both overrides, so no suite builds it inside the
//    measured stage and none can judge a stale one. A MEASURE-tier row since
//    2026-09-26 (`plan::MEASURE_TIER`): `--measure` and `--full` build it, the
//    merge contract does not. The suites' own helper is unchanged — by hand,
//    `targo test -p aterm-conformance` still builds the artifact itself, into
//    the same directory.
// ---------------------------------------------------------------------------

/// The ladder label of the release build — the prime's one child, and the
/// measuring stage's first.
pub const CONFORMANCE_RELEASE_LABEL: &str =
    "targo build --locked --release -p aterm (the artifact paint/spin judge)";

/// The exact command the prime spawns, and the measuring stage re-runs:
/// [`conformance_release_args`] in [`Lane::ConformanceRelease`], from `<root>`
/// like every gate child ([`exec::ExecEnv`]), with the lane's job cap (cargo
/// does not fingerprint the job count, see [`lane_build_jobs`]).
///
/// [`Cmd::demoted`] because this child only COMPILES — the rule that doc states.
#[must_use]
pub fn conformance_release_cmd(ctx: &Ctx) -> Cmd {
    in_lane(
        ctx,
        Lane::ConformanceRelease,
        targo(ctx, conformance_release_args()),
    )
    .demoted()
}

/// A FAILING PRIME IS A FAIL, never a skip: the artifact paint and spin judge
/// could not be built, and the measuring stage's own build would fail the same
/// way. `run_labeled` decides it exactly as every other build child is
/// decided — this stage adds no vocabulary of its own.
fn conformance_release(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        r.skip("conformance release artifact (no targo)");
        return;
    }
    run_labeled(
        ctx,
        r,
        CONFORMANCE_RELEASE_LABEL,
        &conformance_release_cmd(ctx),
    );
}

// ---------------------------------------------------------------------------
// 5c) CONTROL REDRAW CONFORMANCE — the one check that can see a control-socket
//    `select` actually repaint a window. `EventLoop` construction panics off the
//    process main thread and libtest runs every `#[test]` on a spawned one, so
//    every unit test in aterm-gui builds its host with `proxy: None`: a
//    regression that drops the production `EventLoopProxy` compiles and passes
//    the whole default suite. Only a target owning `fn main` can hold a real
//    proxy, which is what `aterm-redraw-conformance` is — and it is behind
//    `required-features`, so this stage is the only thing that ever builds it.
// ---------------------------------------------------------------------------

/// How the ladder reads one harness exit code (`0` pass / `1` fail / `2` NOT RUN,
/// declared in `aterm_gui::control_redraw_conformance`).
///
/// A FUNCTION, and tested, because of `2`. The harness answers it when no event
/// loop is constructible here — headless, no display — and a `2` read as green
/// would restore, one layer up, exactly the false pass this gate exists to
/// remove. It is COULD-NOT-RUN: the run decided nothing, which is neither a pass
/// nor a finding about the tree. Any OTHER code is a finding — the harness drives
/// shipped code, so dying is something the tree did.
#[must_use]
pub fn redraw_outcome(code: Option<i32>) -> (Outcome, String) {
    match code {
        Some(0) => (
            Outcome::Ok,
            "aterm-redraw-conformance: the verb matrix passed against a real proxy, and a select reached the event loop".to_string(),
        ),
        Some(1) => (
            Outcome::Fail(Severity::GateFailed),
            "aterm-redraw-conformance: a check failed, or a redraw the host accepted never arrived".to_string(),
        ),
        Some(2) => (
            Outcome::Fail(Severity::CouldNotRun),
            "aterm-redraw-conformance: NOT RUN — no event loop is constructible here, so nothing was proven about redraws (exit 2, never a pass)".to_string(),
        ),
        Some(c) => (
            Outcome::Fail(Severity::GateFailed),
            format!("aterm-redraw-conformance: unexpected exit {c} (the harness answers only 0/1/2)"),
        ),
        None => (
            Outcome::Fail(Severity::CouldNotRun),
            "aterm-redraw-conformance: no exit status — killed by a signal, or never spawned".to_string(),
        ),
    }
}

fn redraw_conformance(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        // The test compile already reported COULD-NOT-RUN for the same absence;
        // naming it again here keeps the skip counted and the verdict narrowed.
        r.skip("redraw conformance (no targo)");
        return;
    }
    if !build_to_drive(
        ctx,
        r,
        redraw_conformance_build_args(),
        format!("targo build --bin {REDRAW_CONFORMANCE_BIN}"),
        "redraw conformance (the verb matrix against a real proxy)",
    ) {
        return;
    }
    let bin = driver_bin(ctx, REDRAW_CONFORMANCE_BIN);
    if !is_executable_file(&bin) {
        r.cannot_run(format!(
            "redraw conformance: just-built harness missing ({})",
            bin.display()
        ));
        return;
    }
    // Driven as a BINARY, never `targo run`: the driver's lane banner goes to
    // stderr and cargo's own codes would collide with the harness's 0/1/2.
    let out = exec::run(&Cmd::new(&bin), ctx.exec_env());
    r.raw(out.output.as_str());
    let (outcome, label) = redraw_outcome(out.code);
    if r.child_could_not_run(&out, &label) {
        return;
    }
    r.record_child(&out, outcome, label);
}

// ---------------------------------------------------------------------------
// 5d) OBJC LIVE-CLASS AUDIT — the registered class, read back off a real
//    NSWindow's delegate. `vendor/winit`'s `WinitWindowDelegate` is declared by
//    `aterm_objc::declare_class!`, and until this stage NOTHING IN CI READ IT:
//    the seam census checks a mirror class the test file declares, and two
//    compile-verified plants (an argument retyped `Id` -> `Bool`, and
//    `NSWindowDelegate` dropped from `protocols:`) passed that census 6/6 with
//    the build at exit 0 and the driven event log byte-identical.
// ---------------------------------------------------------------------------

fn objc_class_audit(ctx: &Ctx, r: &mut Report) {
    if !cfg!(target_os = "macos") {
        // Not a skip for lack of a tool: the class under audit is declared
        // inside `#[cfg(target_os = "macos")]` and does not exist here at all.
        r.skip("objc live-class audit (macOS only: the audited class is a macOS one)");
        return;
    }
    if !ctx.tools.have_targo() {
        r.skip("objc live-class audit (no targo)");
        return;
    }
    if !build_to_drive(
        ctx,
        r,
        objc_class_audit_build_args(),
        format!("targo build --example {OBJC_CLASS_AUDIT_EXAMPLE}"),
        "objc live-class audit",
    ) {
        return;
    }
    let bin = driver_example(ctx, OBJC_CLASS_AUDIT_EXAMPLE);
    if !is_executable_file(&bin) {
        r.cannot_run(format!(
            "objc live-class audit: just-built auditor missing ({})",
            bin.display()
        ));
        return;
    }
    // Driven as a BINARY, never `targo run`, for the same two reasons as the
    // redraw harness: the driver's lane banner would land in the transcript, and
    // cargo's own exit codes would collide with the auditor's 0/1/2.
    let out = exec::run(&Cmd::new(&bin), ctx.exec_env());
    r.raw(out.output.as_str());
    let (outcome, label) = objc_audit_outcome(out.code);
    if r.child_could_not_run(&out, &label) {
        return;
    }
    r.record_child(&out, outcome, label);
}

// ---------------------------------------------------------------------------
// 5c) THE IME DRIVE — the auditor's twin, asking the other question.
//
//    The audit proves the ported `WinitView` is SHAPED right: 44 registered
//    encodings against the runtime's own authority. It cannot prove the class
//    BEHAVES right, and for `view.rs` that is the question that matters — the
//    eleven `NSTextInputClient` rows are a state machine an input method
//    drives, and a port that registers all eleven correctly and still drops a
//    preedit, mis-clamps a cursor range, or forwards UTF-16 indices where
//    winit's API promises UTF-8 byte offsets passes every shape check in the
//    tree. It also drives the one row whose wrong encoding would not raise:
//    `firstRectForCharacterRange:actualRange:` is how an input method asks
//    where to put its candidate window, so a garbage answer there is a
//    misplaced window rather than an exception.
// ---------------------------------------------------------------------------

fn objc_ime_drive(ctx: &Ctx, r: &mut Report) {
    if !cfg!(target_os = "macos") {
        r.skip("objc IME drive (macOS only: the driven class is a macOS one)");
        return;
    }
    if !ctx.tools.have_targo() {
        r.skip("objc IME drive (no targo)");
        return;
    }
    if !build_to_drive(
        ctx,
        r,
        objc_ime_drive_build_args(),
        format!("targo build --example {OBJC_IME_DRIVE_EXAMPLE}"),
        "objc IME drive",
    ) {
        return;
    }
    let bin = driver_example(ctx, OBJC_IME_DRIVE_EXAMPLE);
    if !is_executable_file(&bin) {
        r.cannot_run(format!(
            "objc IME drive: just-built driver missing ({})",
            bin.display()
        ));
        return;
    }
    // A BINARY, never `targo run`, for the same two reasons as the auditor: the
    // driver lane's banner would land in the transcript, and cargo's own exit
    // codes would collide with the driver's 0/1/2.
    let out = exec::run(&Cmd::new(&bin), ctx.exec_env());
    r.raw(out.output.as_str());
    let (outcome, label) = objc_ime_outcome(out.code);
    if r.child_could_not_run(&out, &label) {
        return;
    }
    r.record_child(&out, outcome, label);
}

// ---------------------------------------------------------------------------
// 5e) THE TOOLBAR DRIVE — the same obligation as 5d, one file over and on the
//    LARGEST ported file in the tree. `crates/aterm-gui/src/toolbar.rs` holds
//    four declared classes and, after W7, every AppKit binding call in the tab
//    strip; nothing in CI drove it. Its `#[cfg(test)] mod objc_tests` checks
//    thirty-two registered encodings against a LITERAL IN THE SAME FILE, and a
//    plant that registered `controlTextDidChange:` as `v@:B` and edited the
//    table to agree left it green. This stage installs the real toolbar in a
//    real NSWindow, enters the registered IMPs through AppKit's dispatch,
//    captures 27 drawing states, and reads all four classes off the live
//    objects. On its first run it found a spurious commit posted by
//    `begin_tab_rename` that left both of the rename editor's exits dead.
// ---------------------------------------------------------------------------

fn objc_toolbar_drive(ctx: &Ctx, r: &mut Report) {
    if !cfg!(target_os = "macos") {
        // Not a skip for lack of a tool: the tab strip and its four declared
        // classes live inside `#[cfg(target_os = "macos")]`.
        r.skip("objc toolbar drive (macOS only: the driven strip is a macOS one)");
        return;
    }
    if !ctx.tools.have_targo() {
        r.skip("objc toolbar drive (no targo)");
        return;
    }
    if !build_to_drive(
        ctx,
        r,
        objc_toolbar_drive_build_args(),
        format!("targo build --example {OBJC_TOOLBAR_DRIVE_EXAMPLE}"),
        "objc toolbar drive",
    ) {
        return;
    }
    let bin = driver_example(ctx, OBJC_TOOLBAR_DRIVE_EXAMPLE);
    if !is_executable_file(&bin) {
        r.cannot_run(format!(
            "objc toolbar drive: just-built driver missing ({})",
            bin.display()
        ));
        return;
    }
    // A BINARY, never `targo run`, for the same two reasons as its siblings:
    // the driver lane's banner would land in the transcript, and cargo's own
    // exit codes would collide with the driver's 0/1/2/3.
    let out = exec::run(&Cmd::new(&bin), ctx.exec_env());
    r.raw(out.output.as_str());
    let (outcome, label) = objc_toolbar_outcome(out.code);
    if r.child_could_not_run(&out, &label) {
        return;
    }
    r.record_child(&out, outcome, label);
}

fn objc_window_drive(ctx: &Ctx, r: &mut Report) {
    if !cfg!(target_os = "macos") {
        r.skip("objc window drive (macOS only: the driven window_delegate.rs is a macOS one)");
        return;
    }
    if !ctx.tools.have_targo() {
        r.skip("objc window drive (no targo)");
        return;
    }
    if !build_to_drive(
        ctx,
        r,
        objc_window_drive_build_args(),
        format!("targo build --example {OBJC_WINDOW_DRIVE_EXAMPLE}"),
        "objc window drive",
    ) {
        return;
    }
    let bin = driver_example(ctx, OBJC_WINDOW_DRIVE_EXAMPLE);
    if !is_executable_file(&bin) {
        r.cannot_run(format!(
            "objc window drive: just-built driver missing ({})",
            bin.display()
        ));
        return;
    }
    // A BINARY, never `targo run`, for the same two reasons as its siblings:
    // the driver lane's banner would land in the transcript, and cargo's own
    // exit codes would collide with the driver's 0/1/2.
    let out = exec::run(&Cmd::new(&bin), ctx.exec_env());
    r.raw(out.output.as_str());
    let (outcome, label) = objc_window_outcome(out.code);
    if r.child_could_not_run(&out, &label) {
        return;
    }
    r.record_child(&out, outcome, label);
}

fn objc_event_drive(ctx: &Ctx, r: &mut Report) {
    if !cfg!(target_os = "macos") {
        r.skip("objc event drive (macOS only: the driven view.rs is a macOS one)");
        return;
    }
    if !ctx.tools.have_targo() {
        r.skip("objc event drive (no targo)");
        return;
    }
    if !build_to_drive(
        ctx,
        r,
        objc_event_drive_build_args(),
        format!("targo build --example {OBJC_EVENT_DRIVE_EXAMPLE}"),
        "objc event drive",
    ) {
        return;
    }
    let bin = driver_example(ctx, OBJC_EVENT_DRIVE_EXAMPLE);
    if !is_executable_file(&bin) {
        r.cannot_run(format!(
            "objc event drive: just-built driver missing ({})",
            bin.display()
        ));
        return;
    }
    // A BINARY, never `targo run`, for the same two reasons as its siblings —
    // and here a third: cargo would turn the driver's signal death into its
    // own exit 101, and the signal IS the finding (see `objc_event_outcome`).
    let out = exec::run(&Cmd::new(&bin), ctx.exec_env());
    r.raw(out.output.as_str());
    let (outcome, label) = objc_event_outcome(out.code);
    if r.child_could_not_run(&out, &label) {
        return;
    }
    r.record_child(&out, outcome, label);
}

fn objc_bound_drive(ctx: &Ctx, r: &mut Report) {
    if !cfg!(target_os = "macos") {
        r.skip("objc bound drive (macOS only: libdispatch's main queue is a Darwin one)");
        return;
    }
    if !ctx.tools.have_targo() {
        r.skip("objc bound drive (no targo)");
        return;
    }
    if !build_to_drive(
        ctx,
        r,
        objc_bound_drive_build_args(),
        format!("targo build -p aterm-objc --example {OBJC_BOUND_DRIVE_EXAMPLE}"),
        "objc bound drive",
    ) {
        return;
    }
    let bin = driver_example(ctx, OBJC_BOUND_DRIVE_EXAMPLE);
    if !is_executable_file(&bin) {
        r.cannot_run(format!(
            "objc bound drive: just-built driver missing ({})",
            bin.display()
        ));
        return;
    }
    // A BINARY, never `targo run`, as its siblings — and this driver
    // re-executes ITSELF as its hang-differential children, by path, so the
    // path it is run from must be the built file and not cargo's runner.
    let out = exec::run(&Cmd::new(&bin), ctx.exec_env());
    r.raw(out.output.as_str());
    let (outcome, label) = objc_bound_outcome(out.code);
    if r.child_could_not_run(&out, &label) {
        return;
    }
    r.record_child(&out, outcome, label);
}

// ---------------------------------------------------------------------------
// 5l) LIVE ATERM LANES (2026-09-26) — the two shell lanes that drive a PRIVATE
//    headless instance of THE one binary, `aterm`, the way a person's tab does:
//    `tools/test-foreground-handback.sh` (a real shell's job control, and the
//    modes a killed foreground program leaves armed) and
//    `tools/test-codex-live-upgrade.sh` (the harness host moving a REAL Codex
//    from the managed store's older build to its current one, in a sandbox
//    with every network denied but one loopback port).
//
//    Until that day no stage, xtask verb, hook or Rust test invoked either,
//    and by hand both said nothing useful: each looks for
//    `<root>/target/debug/aterm`, which no gate lane writes (the workspace
//    build puts it in `target/` only when nothing redirected it, and then
//    under a build that may still be linking), and each answered its absence
//    with a code that is not a failure — `2` from the handback lane, `77`
//    ("SKIP") from the Codex one. Measured on 8c644d49e, run by hand from a
//    checkout: exit 2 and exit 77, i.e. no evidence either way.
//
//    So each lane's stage builds the binary itself — `-p aterm --bin aterm`
//    into the driver lane's dir, a fingerprint no-op after the driver builds
//    row, which compiles it at t0 — runs the lane only if that build
//    succeeded, and HANDS the lane that binary the way its header takes it
//    (`--binary <path>` / the first argument), so neither can fall back to a
//    stale `target/`. And a lane's not-run answer is never a pass
//    ([`live_aterm_outcome`]).
//
//    TWO STAGES, TWO TIERS. The handback lane is the per-commit ladder's last
//    driver-lane row (`StageId::ForegroundHandback`, ~20 s): its verdict is
//    the tree's. The Codex lane is `--full`'s last row, run alone
//    (`StageId::CodexLiveUpgrade`, ~10 min): it reads this machine's managed
//    store (no older Codex, no run) and the vendor's current Codex (whose
//    release-specific internals it asserts), so in the per-commit contract it
//    decided a different thing on each machine and on each night. It sat in
//    the per-commit row for one day; `plan.rs` has the whole reasoning. Neither
//    is removed by a narrowing, for the drivers' reason: the claim is about
//    the shipped binary.
// ---------------------------------------------------------------------------

/// `targo --unverified build -q -p aterm --bin aterm` — the one binary both
/// lanes drive, spelled as the Codex lane's header spells its own build (plus
/// the driver lane's `-q`).
#[must_use]
pub fn live_aterm_build_args() -> Vec<String> {
    [
        "--unverified",
        "build",
        "-q",
        "-p",
        "aterm",
        "--bin",
        "aterm",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// The label of each live stage's first child — the build its lane depends on.
pub const LIVE_ATERM_BUILD_LABEL: &str =
    "targo build -p aterm --bin aterm (the aterm the live lanes drive)";

/// The handback lane. Takes the binary as `--binary <path>`; exits `0` iff no
/// row FAILed, `1` when one did, `2` when the lane could not run.
pub const FOREGROUND_HANDBACK_SUITE: &str = "test-foreground-handback.sh";

/// The Codex branch of the live agent upgrade. Takes the binary as its first
/// argument; exits `0` pass, `1` a check failed, `77` skipped. macOS only.
pub const CODEX_LIVE_UPGRADE_SUITE: &str = "test-codex-live-upgrade.sh";

/// Both live lanes, the roster the fixtures seed from: the handback lane is
/// `StageId::ForegroundHandback`'s, the Codex lane `StageId::CodexLiveUpgrade`'s
/// (`--full` only).
pub const LIVE_ATERM_SUITES: [&str; 2] = [FOREGROUND_HANDBACK_SUITE, CODEX_LIVE_UPGRADE_SUITE];

/// The build of that binary, in the driver lane. Compile-only, so demoted; the
/// lanes themselves RUN code and keep the inherited tier.
#[must_use]
pub fn live_aterm_build_cmd(ctx: &Ctx) -> Cmd {
    driver_build_cmd(ctx, live_aterm_build_args())
}

/// The binary the lanes are handed: this lane's own `debug/aterm`, which each
/// live stage's first child writes. Never `<root>/target/debug/aterm` — the
/// lanes' own default, and the path these rows exist to keep them off.
#[must_use]
pub fn live_aterm_binary(ctx: &Ctx) -> std::path::PathBuf {
    driver_bin(ctx, "aterm")
}

/// One lane's command, with the binary passed the way that lane's header takes
/// it. Anything else a lane needs it makes for itself: a private socket, a
/// scratch HOME, and (the Codex lane) `env -i` around the instance it starts.
///
/// With a `SIGTERM` grace ([`Cmd::term_grace`]): each lane tears down in an
/// EXIT trap — the handback lane its private instance and temp tree, the Codex
/// lane its `/tmp/cxlive.*` tree and, by walking pids, the Codex daemons and
/// background terminals that left the lane's process group on purpose — and a
/// `SIGKILL` of the group would skip it and leave them running.
#[must_use]
pub fn live_aterm_suite_cmd(ctx: &Ctx, name: &str) -> Cmd {
    let cmd = Cmd::new(ctx.tools_dir().join(name)).term_grace(exec::TERM_GRACE);
    let bin = live_aterm_binary(ctx);
    if name == FOREGROUND_HANDBACK_SUITE {
        cmd.arg("--binary").arg(bin)
    } else {
        cmd.arg(bin)
    }
}

/// The reason a lane gave for not running: the text after the LAST `SKIP: `
/// (the Codex lane's `skip()`) or `NOT RUN: ` (the handback lane's
/// `not_run()`) line it printed — each prints exactly one, then exits.
fn not_run_reason(transcript: &str) -> Option<&str> {
    transcript
        .lines()
        .rev()
        .find_map(|l| {
            let l = l.trim();
            l.strip_prefix("SKIP: ")
                .or_else(|| l.strip_prefix("NOT RUN: "))
        })
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// How the ladder reads a live lane's exit. A FUNCTION, and tested, because of
/// the not-run codes: the handback lane's `2` and the Codex lane's `77` are
/// what each answered when it had no binary, and a gate that read either as
/// green would pass a lane that drove nothing — the state these rows replace.
///
/// The two codes are read differently because the two lanes sit in different
/// tiers. The handback lane's `2` is COULD NOT RUN, the redraw harness's
/// reading of its own `2`: the per-commit run decided nothing, which is neither
/// a pass nor a finding about the tree. That reading is honest only because
/// the lane now keeps `2` for exactly that — no binary, an unknown argument, a
/// socket path too long, no python3 — and answers an instance that exits or
/// never answers with a FAIL row and `1` (until 2026-09-26 it answered that
/// with `2` too, so a tree whose `aterm --headless` crashed at startup read as
/// a broken machine; the control-socket smoke has always called it a FAIL). The Codex lane's `77`, once the binary
/// is handed over, means a prerequisite of THIS MACHINE's is absent — no older
/// managed Codex to upgrade from, no python3 — and in `--full` that is what the
/// trust-mc floor's absent prover is: a NAMED SKIP, counted, printed with the
/// lane's own reason so the verdict names the remedy, and forfeiting the run's
/// contract claim. Never a pass either way. Any other code is a finding — a
/// lane that dies mid-way died on something the shipped binary did, or on its
/// own script, and both belong to the tree.
#[must_use]
pub fn live_aterm_outcome(name: &str, code: Option<i32>, transcript: &str) -> (Outcome, String) {
    let handback = name == FOREGROUND_HANDBACK_SUITE;
    let not_run = if handback { 2 } else { 77 };
    match code {
        Some(0) => (Outcome::Ok, name.to_string()),
        Some(1) => (
            Outcome::Fail(Severity::GateFailed),
            format!("{name}: a check failed against the live aterm — its rows above say which"),
        ),
        Some(2) if handback => (
            Outcome::Fail(Severity::CouldNotRun),
            format!(
                "{name}: NOT RUN — {} (exit 2, never a pass)",
                not_run_reason(transcript).unwrap_or("the lane could not run and did not say why")
            ),
        ),
        Some(77) if !handback => (
            Outcome::Skip,
            format!(
                "{name}: NOT RUN — {} (exit 77: this machine lacks a prerequisite; a named skip, \
                 never a pass)",
                not_run_reason(transcript).unwrap_or("the lane skipped without saying why")
            ),
        ),
        Some(c) => (
            Outcome::Fail(Severity::GateFailed),
            format!("{name}: unexpected exit {c} (the lane answers only 0, 1 and {not_run})"),
        ),
        None => (
            Outcome::Fail(Severity::CouldNotRun),
            format!("{name}: no exit status — killed by a signal, or never spawned"),
        ),
    }
}

/// Why a live lane runs on macOS only — the reason its off-macOS skip names.
///
/// The Codex lane cannot run anywhere else. The handback lane has simply never
/// RUN anywhere else, and one of its rows is known to be macOS's answer rather
/// than the tree's: it boots `/bin/bash` and expects `bracketed_paste=false`
/// after the handback, because macOS's `/bin/bash` is 3.2, which has no 2004 —
/// while bash 5.1 and later arm bracketed paste by default, which is what a
/// Linux host's `/bin/bash` is. So off macOS that row would FAIL on every
/// commit, and the rest of the lane (`/bin/zsh`, `tput`, the kill notices it
/// matches) is unmeasured there. A named skip says so; running it would claim
/// coverage nobody has seen.
#[must_use]
pub fn live_aterm_macos_only(name: &str) -> &'static str {
    if name == FOREGROUND_HANDBACK_SUITE {
        "the lane has been measured nowhere else, and its bash row expects macOS's \
         /bin/bash 3.2, which has no bracketed paste, where bash 5.1+ arms it by default"
    } else {
        "the instance runs under sandbox-exec, and the ps stand-in reads sysctl's kinfo_proc"
    }
}

/// The per-commit live row: the foreground handback.
fn foreground_handback(ctx: &Ctx, r: &mut Report) {
    live_aterm_stage(ctx, r, FOREGROUND_HANDBACK_SUITE);
}

/// The `--full` live row: the Codex live upgrade.
fn codex_live_upgrade(ctx: &Ctx, r: &mut Report) {
    live_aterm_stage(ctx, r, CODEX_LIVE_UPGRADE_SUITE);
}

/// One live lane's stage, in order: the suite exists, the platform, a
/// toolchain, the build of the binary, the binary, the lane.
fn live_aterm_stage(ctx: &Ctx, r: &mut Report, name: &str) {
    let t = ctx.tools_dir().join(name);
    if !is_executable_file(&t) {
        r.cannot_run(format!(
            "{name} missing or not executable ({})",
            t.display()
        ));
        return;
    }
    if !cfg!(target_os = "macos") {
        r.skip(format!(
            "{name} (macOS only: {})",
            live_aterm_macos_only(name)
        ));
        return;
    }
    if !ctx.tools.have_targo() {
        // Counted and named, as the atpkg pack's: with no toolchain nothing can
        // build the binary, and the lane's coverage is absent from the run.
        r.skip(format!(
            "{name} (no targo — nothing can build the aterm it drives)"
        ));
        return;
    }
    if !run_labeled(ctx, r, LIVE_ATERM_BUILD_LABEL, &live_aterm_build_cmd(ctx)) {
        // Not a skip: nothing was absent. Running the lane anyway would drive
        // whatever binary an earlier run left, which is the stale path this row
        // exists to close — and the lane it keeps from running decided nothing
        // ([`NOT_RUN_BEHIND`]).
        r.cannot_run(format!(
            "{name} — not run: the aterm build above failed, so the lane has no fresh binary to drive{}",
            NOT_RUN_BEHIND
        ));
        return;
    }
    let bin = live_aterm_binary(ctx);
    if !is_executable_file(&bin) {
        r.cannot_run(format!(
            "{name}: the just-built aterm is missing ({})",
            bin.display()
        ));
        return;
    }
    let out = exec::run(&live_aterm_suite_cmd(ctx, name), ctx.exec_env());
    r.raw(out.output.as_str());
    let (outcome, label) = live_aterm_outcome(name, out.code, &out.output);
    if r.child_could_not_run(&out, &label) {
        return;
    }
    r.record_child(&out, outcome, label);
}

// ---------------------------------------------------------------------------
// 6b) --full ONLY: trust-mc / Kani BMC floor.
//
//    The real driver is `cargo trust-mc --config-free --harness <name>` via
//    scripts/verify-kani-proofs.sh. There is no `trust-mc verify` subcommand, and
//    stock cargo-kani/CBMC is banned — verification is discharged by trust-mc + ay.
//    An unavailable toolchain is reported PROMINENTLY and skipped, exactly as the
//    --full contract promises; it is never described as discharged.
// ---------------------------------------------------------------------------
/// The sentence `xtask gate cells` prints when it RAN but one or more cells were
/// never compiled. ONE definition, in the crate both readers already depend on:
/// `crates/xtask/src/gate.rs` formats it, [`cells_outcome`] matches it. Two
/// copies of a sentinel is how a sentinel stops being one.
pub const CELLS_NOT_PROVEN: &str = "gate cells: NOT PROVEN";

/// How the ladder reads `gate cells`: the exit code AND the transcript.
///
/// A FUNCTION, and tested, for the reason [`redraw_outcome`] is — plus one this
/// stage owns. `gate cells` MAY NOT answer an uninstalled std with a non-zero
/// exit: a cross std is a property of the box, no change to this repository can
/// conjure one, and a red nobody can clear is a red nobody reads. So it exits 0
/// and says so IN WORDS. Read on the exit code alone — which is what this stage
/// did until 2026-09-17 — a run where NO COMPILER STARTED was byte-identical to
/// the matrix discharged: measured that day, five `SKIPPED(no-std)` rows,
/// `gate cells: GREEN — all 5 cells type-check`, exit 0, `ok gate cells` on the
/// ladder and `merge contract satisfied` underneath it.
///
/// [`Outcome::Skip`] is the honest reading: counted, NAMED, exit code untouched,
/// and the merge-contract sentence withheld by
/// [`crate::verdict::discharges_merge_contract`] because a skipped stage claims
/// nothing.
#[must_use]
pub fn cells_outcome(verb: &str, ok: bool, transcript: &str) -> (Outcome, String) {
    if !ok {
        return (Outcome::Fail(Severity::GateFailed), verb.to_string());
    }
    // AT COLUMN 0, never `contains`. The gate prints its verdict unindented and
    // everything it echoes from a compiler is indented or carries cargo's own
    // prefix, so an anchored match cannot be fed a line of TREE TEXT — the same
    // restriction `guard_announced_skip` is built on, and for the same reason.
    if transcript
        .lines()
        .any(|line| line.starts_with(CELLS_NOT_PROVEN))
    {
        return (
            Outcome::Skip,
            format!(
                "{verb} (a forge cell had no installed std — NOTHING was compiled for it; the \
                 NOT PROVEN line above names which)"
            ),
        );
    }
    (Outcome::Ok, verb.to_string())
}

/// `--full` only: every forge cell type-checked FOR ITS OWN TRIPLE.
///
/// The rest of this gate compiles aterm for one target. aterm ships five, and
/// until 2026-09-01 the other four were held by source reading — which is where
/// both defects the `once_cell` judge found had been living. `xtask gate cells`
/// runs a real compiler per triple, on a toolchain that carries that std, from
/// a cwd and into a target directory OUTSIDE this repo. A cell whose toolchain
/// is not installed SKIPS inside the verb and says out loud that nothing was
/// compiled for it; a cell that runs and fails is a FAILURE, never re-read as a
/// skip. AND THE SKIP REACHES THIS LADDER — see [`cells_outcome`]. It did not
/// until 2026-09-17: the verb said "nothing was compiled" on its own stderr and
/// then exited 0, and this stage, reading only the code, wrote `ok gate cells`.
///
/// AND IT READS THIS REPO'S OWN CODE ON ALL FIVE. For the first day of its life
/// the verb was GREEN on linux and win while neither cell had type-checked one
/// line of aterm's first-party crates: `ring` and `zstd-sys` bundle C, their
/// build scripts could not run for those triples, and the excuse for that took
/// eighteen crates with it. They are SHIMMED now (`tools/cross-cell-gate.tsv`,
/// `cshim` rows), and each cell FAILS if any in-repo package in its graph goes
/// unread — an obligation with no escape hatch in the policy file.
fn cross_cells(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        r.skip("cross-cell type-check (no targo)");
        return;
    }
    let out = exec::run(&targo(ctx, xtask_gate_args("cells")), ctx.exec_env());
    r.raw(out.output.as_str());
    if r.child_could_not_run(&out, "gate cells") {
        return;
    }
    let (outcome, label) = cells_outcome("gate cells", out.ok, out.output.as_str());
    r.record_child(&out, outcome, label);
}

/// Every gate run: the cells NO fleet box hosts (`xtask gate
/// cells-foreign` — win, the two wasm32 cells, linux-arm, win-arm on today's
/// matrix), type-checked for their own triples. Decided 2026-09-25 under the
/// owner's standing direction, because the class it guards has shipped: the
/// Windows binary did not build from 2026-09-12 (v0.82.0) to 2026-09-14 and no
/// gate ran that said so, `gate cells` being behind `--full` — and it caught
/// the next one on 2026-09-26 (the harness upgrade drives' Unix-only process
/// group, merged to main red for both Windows cells).
///
/// THE COST, re-measured 2026-09-27 on this M5 Max at load average 40-65,
/// with `CARGO_INCREMENTAL=0` as every gate child has it: 432 s cold into 2.6
/// GiB, 8 s warm with nothing changed, 278 s after `touch
/// crates/aterm-grid/src/lib.rs` (a hand run with incremental on: 75 s after
/// the same touch, into 12 GiB). It sits in the xtask lane, which nothing waits
/// for since the test run waits only for the driver lane, and inherits that
/// lane's job cap. It was off the test run's critical path even while the run
/// still waited for this lane: after an edit to aterm-grid's lib.rs a
/// merge-contract run's cells ended at +275 s, its build at +513 s, and the
/// test run started at +931 s, when the driver lane finished (2026-09-27,
/// `--timings`). A snapshot's run builds into
/// the snapshot's own CELLS LANE beside it ([`foreign_cells_cmd`]), which the
/// disk preflight counts and caps with the other lanes. The mac and
/// x86_64-linux cells stay with the native lanes of the boxes that host them
/// and with `--full`'s whole matrix.
///
/// PREREQUISITE: rustup's `stable` with the four foreign std targets. Without
/// them `xtask` exits 0 naming the missing std, this stage records a SKIP, and
/// the verdict withholds the merge-contract sentence on that box.
fn foreign_cells(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        r.skip("gate cells-foreign (no targo)");
        return;
    }
    let out = exec::run(&foreign_cells_cmd(ctx), ctx.exec_env());
    r.raw(out.output.as_str());
    if r.child_could_not_run(&out, "gate cells-foreign") {
        return;
    }
    let (outcome, label) = cells_outcome("gate cells-foreign", out.ok, out.output.as_str());
    r.record_child(&out, outcome, label);
}

/// `xtask gate cells-foreign`, pointed at the run's CELLS LANE
/// ([`crate::disk::cells_lane`]) through `$ATERM_CELL_TARGET_DIR`, so what it
/// writes is on the snapshot's volume, counted by the disk preflight and
/// removed by its cap. Without it the stage wrote into `xtask`'s per-checkout
/// default under `~/.cache/aterm/cells`, which no budget counted.
fn foreign_cells_cmd(ctx: &Ctx) -> Cmd {
    let cmd = xtask_cmd(ctx, xtask_gate_args("cells-foreign"));
    match crate::disk::cells_lane(&ctx.root) {
        Some(cells) => cmd.env("ATERM_CELL_TARGET_DIR", cells),
        None => cmd,
    }
}

/// Every gate run: `xtask gate forge` — the third-party surface (vendored
/// forks and first-party patch targets live on every cell with no unpatched
/// sibling, carved paths absent, provenance attested, the `[OB-14]` budget
/// ratchet, `[OB-16]` mirror honesty, `[OB-17]` the fork ledger). Decided
/// 2026-09-25 under the owner's standing direction: a flat ~9-14 s with no
/// compiler and no network, in the xtask lane beside the other gate verbs —
/// and a gate that only `--full` runs is a gate nothing automatic runs, which
/// is how `gate cells` missed a Windows break for two days.
fn forge_gate(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        r.skip("gate forge (no targo)");
        return;
    }
    run_labeled(
        ctx,
        r,
        "gate forge",
        &xtask_cmd(ctx, xtask_gate_args("forge")),
    );
}

fn kani_floor(ctx: &Ctx, r: &mut Report) {
    let gate = ctx.root.join("scripts/verify-kani-proofs.sh");
    let mc_root = trust_mc_sysroot(&ctx.env);
    let ay_dir = ay_bin_dir(&ctx.env);

    if !is_executable_file(&gate) {
        r.cannot_run(format!(
            "verify-kani-proofs.sh missing or not executable ({})",
            gate.display()
        ));
        return;
    }
    if !is_executable_file(&mc_root.join("bin/cargo-trust-mc"))
        && !is_executable_file(&mc_root.join("bin/trust-mc-driver"))
    {
        r.raw("  NOTICE: Tier-2 trust-mc/Kani obligations were NOT RUN: trust-mc is unavailable");
        r.raw(format!(
            "          at {} (the embedded + ty tiers still ran).",
            mc_root.display()
        ));
        r.raw("          fix: `aterm pkg install trust-mc` (`aterm pkg doctor --verbose` names the store);");
        r.raw("          or set TRUST_MC_SYSROOT at a from-source build-trust-mc sysroot.");
        r.skip("trust-mc / Kani BMC floor (tool unavailable; `aterm pkg install trust-mc`)");
        return;
    }
    if !is_executable_file(&ay_dir.join("ay")) && !have_on_path("ay", &ctx.path_env) {
        r.raw("  NOTICE: Tier-2 trust-mc/Kani obligations were NOT RUN: ay is unavailable");
        r.raw(format!(
            "          at {} and on PATH (the embedded + ty tiers still ran).",
            ay_dir.display()
        ));
        r.raw(
            "          fix: `aterm pkg install ay`; or set AY_BIN_DIR at a from-source ay build.",
        );
        r.skip("trust-mc / Kani BMC floor (solver unavailable; `aterm pkg install ay`)");
        return;
    }
    for krate in KANI_CRATES {
        // NOT `run_labeled`: that decides from the exit code alone, and this
        // script exits 0 BOTH when it discharged proofs and when it discharged
        // NONE — the standing policy is that an all-inconclusive run is a
        // trust-mc modelling limitation, not an aterm defect, so it must not
        // fail the build. The old code therefore recorded a lane that proved
        // ZERO harnesses as `ok`, which every reader takes for coverage. The
        // script states which case it is on its last line; read that, so a
        // zero-proof run becomes the NAMED skip it is (counted by the tally,
        // which forfeits the merge-contract claim) and no run changes verdict.
        let out = exec::run(&kani_cmd(&gate, krate, &mc_root, &ay_dir), ctx.exec_env());
        r.raw(out.output.as_str());
        if r.child_could_not_run(&out, &format!("verify-kani-proofs.sh ({krate})")) {
            continue;
        }
        let (outcome, why) = kani_floor_outcome(out.ok, &out.output);
        r.record_child(
            &out,
            outcome,
            format!("verify-kani-proofs.sh ({krate}){why}"),
        );
    }
}

/// `scripts/verify-kani-proofs.sh` exit 0 does NOT mean "proofs were
/// discharged": by that script's deliberate policy an all-inconclusive run
/// exits 0 too. The two are distinguished only by its verdict line — `PASS: N
/// config-free Kani proof(s) discharged` versus `NOT PROVED (… NOTHING
/// discharged)` — so the ladder row has to be decided from the output, not from
/// the status. Anything else at exit 0 (a rewritten script, a truncated run)
/// asserted nothing about proofs and is reported the same fail-open-but-honest
/// way: a skip, which the tally names and the verdict subtracts from the claim.
/// Which runs FAIL is unchanged — nonzero is still a finding.
fn kani_floor_outcome(ok: bool, output: &str) -> (Outcome, &'static str) {
    if !ok {
        return (Outcome::Fail(Severity::GateFailed), "");
    }
    let said = |marker: &str| output.lines().any(|l| l.starts_with(marker));
    if said("PASS:") {
        (Outcome::Ok, "")
    } else if said("NOT PROVED") {
        (
            Outcome::Skip,
            " (0 proofs discharged; nothing verified here)",
        )
    } else {
        (
            Outcome::Skip,
            " (exited 0 without a proof-count verdict line; nothing claimed)",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EnvSnapshot;
    use crate::cli::Mode;
    use std::path::PathBuf;

    fn ctx(scope: Scope) -> Ctx {
        Ctx::new(
            PathBuf::from("/repo"),
            Mode::Fast,
            scope,
            EnvSnapshot::default(),
            PathBuf::from("/tmp"),
        )
    }

    /// A SNAPSHOT'S FOREIGN CELLS BUILD IN ITS CELLS LANE (2026-09-27): the
    /// stage's child carries `$ATERM_CELL_TARGET_DIR` naming the lane beside
    /// the snapshot — the directory the disk preflight measures and its cap
    /// removes — and never `xtask`'s own default under the caller's cache.
    #[test]
    fn a_snapshots_foreign_cells_build_in_its_cells_lane() {
        let target_dir = |c: &Cmd| {
            c.envs
                .iter()
                .find(|(k, _)| k == "ATERM_CELL_TARGET_DIR")
                .map(|(_, v)| PathBuf::from(v))
        };
        let snap = Ctx::new(
            PathBuf::from("/w/aterm-verify.noindex"),
            Mode::Fast,
            Scope::workspace(),
            EnvSnapshot::default(),
            PathBuf::from("/tmp"),
        )
        .in_snapshot_of(
            PathBuf::from("/w/aterm"),
            crate::identity::TreeState {
                head: "0".repeat(40),
                dirty: std::collections::BTreeMap::new(),
            },
            Vec::new(),
        );
        assert_eq!(
            target_dir(&foreign_cells_cmd(&snap)),
            Some(PathBuf::from("/w/aterm-verify-cells.noindex"))
        );
        assert_eq!(
            crate::disk::cells_lane(&snap.root),
            target_dir(&foreign_cells_cmd(&snap)),
            "the stage builds where the preflight measures"
        );
    }

    /// A PRIME THAT CANNOT BUILD IS A FAIL, NOT A SKIP. Paint and spin judge
    /// that artifact and refuse to run without one, so a failed prime is a
    /// finding the measuring stage would reach anyway — demoting it to a skip
    /// would narrow the verdict for a failure the gate really did find.
    #[test]
    fn a_failed_conformance_prime_is_a_finding_and_a_missing_targo_is_a_skip() {
        let c = ctx(Scope::workspace());
        assert!(!c.tools.have_targo());
        let mut r = Report::new("prime");
        conformance_release(&c, &mut r);
        let (outcome, label) = r.outcomes().next().expect("a decision");
        assert_eq!(outcome, crate::Outcome::Skip, "{label}");
        assert!(label.contains("no targo"), "{label}");

        // With a driver that exits non-zero, the same stage FAILS — the
        // ordinary `decide_child` verdict, no vocabulary of its own.
        let mut c = ctx(Scope::workspace());
        c.root = std::env::temp_dir();
        c.scratch = std::env::temp_dir();
        c.tools.targo = PathBuf::from("/usr/bin/false");
        c.tools.refused = None;
        assert!(
            c.tools.have_targo(),
            "the stand-in driver must be reachable"
        );
        let mut r = Report::new("prime");
        conformance_release(&c, &mut r);
        let (outcome, label) = r.outcomes().next().expect("a decision");
        assert_eq!(
            outcome,
            crate::Outcome::Fail(crate::Severity::GateFailed),
            "{label}"
        );
        assert!(label.contains(CONFORMANCE_RELEASE_LABEL), "{label}");
    }

    /// THE MEASURING STAGE JUDGES THE ARTIFACT ITS OWN FIRST CHILD LEAVES
    /// (2026-09-27). Paint and spin take a prebuilt release `aterm` from
    /// `ATERM_PAINT_BIN` (spin: `ATERM_SPIN_BIN` first) and build one
    /// themselves only when neither is set. The stage runs the prime's build
    /// first — a fingerprint check after the prime row — and hands both
    /// variables that build's `release/aterm`, in the prime's lane; a failed
    /// build runs no suite (one FAIL, and a `not run:` line), so nothing
    /// judges an artifact a previous run left. The suites' own sources read
    /// those names (read from the tree, so a rename reddens here).
    #[test]
    fn the_measuring_stage_hands_paint_and_spin_the_artifact_its_own_build_leaves() {
        let c = ctx(Scope::workspace());
        let lane = lane_dir(&c, Lane::ConformanceRelease).expect("a cargo lane");
        let bin = conformance_release_binary(&c);
        assert_eq!(bin, lane.join("release").join("aterm"));
        let env_of = |cmd: &Cmd, key: &str| {
            cmd.envs
                .iter()
                .find(|(k, _)| k.to_str() == Some(key))
                .map(|(_, v)| PathBuf::from(v))
        };
        assert_eq!(
            env_of(&conformance_release_cmd(&c), "CARGO_TARGET_DIR"),
            Some(lane)
        );
        let (_, run) = measuring_cmd(&c, false);
        for var in ["ATERM_PAINT_BIN", "ATERM_SPIN_BIN"] {
            assert_eq!(env_of(&run, var), Some(bin.clone()), "{var}");
        }
        let tests =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../aterm-conformance/tests");
        for (file, needle) in [
            (
                "paint/measuring.rs",
                r#"release_bin(&root, &["ATERM_PAINT_BIN"])"#,
            ),
            (
                "spin/measuring.rs",
                r#"release_bin(&root, &["ATERM_SPIN_BIN", "ATERM_PAINT_BIN"])"#,
            ),
            ("support/mod.rs", "for var in overrides {"),
        ] {
            let text = std::fs::read_to_string(tests.join(file)).expect(file);
            assert!(text.contains(needle), "{file}: {needle}");
        }

        // A failed build runs no suite: exactly one FAIL, the build's.
        let mut c = ctx(Scope::workspace());
        c.root = std::env::temp_dir();
        c.scratch = std::env::temp_dir();
        c.tools.targo = PathBuf::from("/usr/bin/false");
        c.tools.refused = None;
        let mut r = Report::new("measuring");
        measuring_tests(&c, &mut r);
        let rendered = r.render();
        let t = crate::ladder::tally(std::slice::from_ref(&r));
        assert_eq!(t.gate_failures.len(), 1, "{rendered}");
        assert!(
            t.gate_failures[0].contains(CONFORMANCE_RELEASE_LABEL),
            "{rendered}"
        );
        assert!(
            rendered.contains("  not run: targo test --workspace --test paint --test spin"),
            "{rendered}"
        );
    }

    #[test]
    fn libc_oracle_owns_an_absolute_target_dir_and_suppresses_python_bytecode() {
        let c = ctx(Scope::workspace());
        let cmd = libc_oracle_cmd(&c).expect("the absolute fixture root resolves");
        assert_eq!(cmd.program, PathBuf::from("/repo/libc-oracle/run.sh"));
        assert_eq!(
            cmd.envs,
            [
                ("CARGO_TARGET_DIR".into(), "/repo/libc-oracle/target".into()),
                ("PYTHONDONTWRITEBYTECODE".into(), "1".into()),
            ],
            "the required lane names its own target dir"
        );

        let mut relative = ctx(Scope::workspace());
        relative.root = PathBuf::from("relative-repo");
        let cmd = libc_oracle_cmd(&relative).expect("a relative repo root can be absolutized");
        let target = &cmd.envs[0].1;
        assert!(
            PathBuf::from(target).is_absolute(),
            "run.sh's children run from different cwds, so this cannot be relative: {target:?}"
        );
        assert!(
            PathBuf::from(target).ends_with("relative-repo/libc-oracle/target"),
            "the lane must remain rooted in its repository: {target:?}"
        );
    }

    #[test]
    fn libc_oracle_exit_three_is_could_not_run_not_a_finding() {
        assert_eq!(libc_oracle_outcome(Some(0)), Outcome::Ok);
        assert_eq!(
            libc_oracle_outcome(Some(1)),
            Outcome::Fail(Severity::GateFailed)
        );
        assert_eq!(
            libc_oracle_outcome(Some(3)),
            Outcome::Fail(Severity::CouldNotRun)
        );
        assert_eq!(
            libc_oracle_outcome(None),
            Outcome::Fail(Severity::CouldNotRun)
        );
        assert_eq!(
            libc_oracle_outcome(Some(2)),
            Outcome::Fail(Severity::GateFailed),
            "an undeclared driver status is not allowed to masquerade as an environment verdict"
        );
    }

    #[test]
    fn a_machine_with_no_doc_driver_anywhere_is_diagnosed_on_the_doctests_row() {
        // targo exists, trustdoc does not, and nothing on the children's PATH
        // answers to the bare name. Since 2026-09-13 the doctests stage is the
        // ONLY doctest runner, so it says COULD-NOT-RUN with the remedy before
        // spawning anything. The test stage's two children (`--no-run`,
        // `--tests`) spawn no rustdoc, so they simply run — `/usr/bin/true`
        // stands in for targo, and two Ok outcomes prove both ran.
        let spec = |id| StageSpec {
            id,
            title: "t".into(),
            lane: crate::plan::Lane::MainTarget,
            exclusive: false,
            after_lanes: Vec::new(),
        };
        let no_driver = |scope| {
            let mut c = ctx(scope);
            c.tools.targo = PathBuf::from("/usr/bin/true");
            c.tools.trustdoc = PathBuf::from("/nonexistent/trustdoc");
            c.path_env = "/nonexistent-dir".into();
            // These cases exec, so the child needs a real cwd.
            c.root = std::env::temp_dir();
            c.scratch = std::env::temp_dir();
            c
        };
        let cc = no_driver(Scope::workspace());

        let r = run_stage(&cc, &spec(StageId::Test));
        let outcomes: Vec<_> = r.outcomes().collect();
        assert_eq!(
            outcomes,
            [
                (Outcome::Ok, "targo test --workspace --no-run"),
                (Outcome::Ok, "targo test --workspace --tests"),
            ]
        );

        let r = run_stage(&cc, &spec(StageId::Doctests));
        let outcomes: Vec<_> = r.outcomes().collect();
        assert_eq!(outcomes.len(), 1, "doctests decided once");
        assert_eq!(outcomes[0].0, Outcome::Fail(Severity::CouldNotRun));
        assert!(
            outcomes[0].1.contains("~/.local/bin/trustdoc"),
            "the diagnosis names the remedy: {}",
            outcomes[0].1
        );

        // A cone with no lib target compiles no doctests, so the doctests
        // stage skips on its lib-target guard instead of blaming a tool the
        // run never needed.
        let cb = no_driver(Scope::changed("main", vec!["xtask".into()], false));
        let r = run_stage(&cb, &spec(StageId::Doctests));
        let outcomes: Vec<_> = r.outcomes().collect();
        assert_eq!(outcomes.len(), 1, "bin-only doctests decided once");
        assert_eq!(outcomes[0].0, Outcome::Skip, "{}", outcomes[0].1);

        // `--scope <crate>` answers the lib question from the member's own
        // manifest: a bin-only scope skips, and the same scope with a lib is a
        // real diagnosis.
        let tmp = crate::mktemp_dir("atv-doc-scope").expect("mktemp");
        let bin_only = tmp.join("crates/binonly");
        std::fs::create_dir_all(bin_only.join("src")).unwrap();
        std::fs::write(
            bin_only.join("Cargo.toml"),
            "[package]\nname = \"binonly\"\n",
        )
        .unwrap();
        let mut cx = no_driver(Scope::Crate("binonly".into()));
        cx.root = tmp.clone();
        let r = run_stage(&cx, &spec(StageId::Doctests));
        let outcomes: Vec<_> = r.outcomes().collect();
        assert_eq!(outcomes[0].0, Outcome::Skip, "{}", outcomes[0].1);

        std::fs::write(bin_only.join("src/lib.rs"), "").unwrap();
        let r = run_stage(&cx, &spec(StageId::Doctests));
        let outcomes: Vec<_> = r.outcomes().collect();
        assert_eq!(
            outcomes[0].0,
            Outcome::Fail(Severity::CouldNotRun),
            "{}",
            outcomes[0].1
        );
        // …while the test stage runs on the same scope: it needs no driver.
        let r = run_stage(&cx, &spec(StageId::Test));
        assert!(r.outcomes().all(|(o, _)| o == Outcome::Ok));
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn the_doc_driver_rule_pins_stage2_first_and_diagnoses_absence() {
        // The pinned driver always wins (the gate runs THE toolchain's
        // drivers, and its RUSTDOC binding overrides even a caller's export);
        // the caller's export outranks the PATH walk exactly as cargo ranks
        // env over config; a farm link keeps an older stage2 running
        // fail-closed; and nothing-anywhere is a diagnosis, never a raw exec
        // death after the unit tests already passed.
        assert_eq!(doc_driver_from(true, true, true), DocDriver::Stage2);
        assert_eq!(doc_driver_from(true, false, false), DocDriver::Stage2);
        assert_eq!(doc_driver_from(false, true, true), DocDriver::Ambient);
        assert_eq!(doc_driver_from(false, true, false), DocDriver::Ambient);
        assert_eq!(doc_driver_from(false, false, true), DocDriver::BarePath);
        assert_eq!(doc_driver_from(false, false, false), DocDriver::Absent);
    }

    /// THE PIN for the truncating test stage (2026-09-10). `targo test` stops
    /// scheduling the moment ONE test binary fails, so a workspace run reports
    /// on a PREFIX of the tree while reading like a statement about all of it.
    /// Measured: three consecutive gates reported on 41 of ~319 binaries
    /// because `aterm-conformance` sorts early and its paint rows are
    /// load-sensitive — `aterm-forge`'s six failing baseline tests never ran,
    /// and their silence read as green for two days. `--no-fail-fast` is the
    /// coverage flag, exactly as `--keep-going` is for tippy; it widens what
    /// the stage sees and changes nothing about the verdict, because cargo
    /// still exits non-zero when anything failed.
    #[test]
    fn every_test_argv_carries_no_fail_fast_so_one_binary_cannot_hide_the_rest() {
        for argv in [
            test_compile_args(&Scope::workspace()),
            test_run_args(&Scope::workspace()),
            test_compile_args(&Scope::crate_only("aterm-grid")),
            test_run_args(&Scope::crate_only("aterm-grid")),
            test_compile_args(&Scope::changed("main", vec!["aterm-gui".into()], true)),
            test_run_args(&Scope::changed("main", vec!["aterm-gui".into()], true)),
            measuring_args(&Scope::workspace()),
            measuring_args(&Scope::crate_only("aterm-conformance")),
            deadline_args(&Scope::workspace()),
            deadline_args(&Scope::crate_only("aterm-update")),
            doctest_args(&Scope::workspace()),
            doctest_args(&Scope::crate_only("aterm-grid")),
        ] {
            assert!(
                argv.iter().any(|a| a == "--no-fail-fast"),
                "a test argv without --no-fail-fast reports a PREFIX of its \
                 scope: {argv:?}"
            );
        }
    }

    #[test]
    fn a_scope_reaches_the_test_doctest_and_lint_argv_together() {
        let s = Scope::crate_only("aterm-grid");
        assert_eq!(
            test_compile_args(&s),
            [
                "--unverified",
                "test",
                "-p",
                "aterm-grid",
                "--no-fail-fast",
                "--no-run",
                "--message-format=json-render-diagnostics"
            ]
        );
        assert_eq!(
            doctest_args(&s),
            [
                "--unverified",
                "test",
                "--doc",
                "-p",
                "aterm-grid",
                "--no-fail-fast"
            ]
        );
        assert_eq!(
            tippy_args(&s),
            [
                "-p",
                "aterm-grid",
                "--all-targets",
                "--keep-going",
                "--",
                "-D",
                "warnings"
            ]
        );
    }

    #[test]
    fn a_change_scope_reaches_the_same_three_argvs_as_one_dash_p_per_crate() {
        let s = Scope::changed("main", vec!["aterm-grid".into(), "aterm-gui".into()], true);
        assert_eq!(
            test_compile_args(&s),
            [
                "--unverified",
                "test",
                "-p",
                "aterm-grid",
                "-p",
                "aterm-gui",
                "--no-fail-fast",
                "--no-run",
                "--message-format=json-render-diagnostics"
            ]
        );
        assert_eq!(
            doctest_args(&s),
            [
                "--unverified",
                "test",
                "--doc",
                "-p",
                "aterm-grid",
                "-p",
                "aterm-gui",
                "--no-fail-fast"
            ]
        );
        assert_eq!(
            tippy_args(&s),
            [
                "-p",
                "aterm-grid",
                "-p",
                "aterm-gui",
                "--all-targets",
                "--keep-going",
                "--",
                "-D",
                "warnings"
            ]
        );
    }

    #[test]
    fn an_empty_change_selection_compiles_nothing_in_any_of_those_stages() {
        // The guard is in `run_scoped`, so it has to hold for every stage that
        // uses it — a stage that forgot would run the `--workspace` fallback and
        // build the whole tree under the label `<no crates selected>`.
        let c = ctx(Scope::changed("main", vec![], true));
        for stage in [
            StageId::TestCompile,
            StageId::Test,
            StageId::Doctests,
            StageId::Tippy,
        ] {
            let mut cc = ctx(c.scope.clone());
            cc.tools.targo = PathBuf::from("/bin/sh");
            cc.tools.tippy = Some(PathBuf::from("/bin/sh"));
            let r = run_stage(
                &cc,
                &StageSpec {
                    id: stage,
                    title: "t".into(),
                    lane: crate::plan::Lane::MainTarget,
                    exclusive: false,
                    after_lanes: Vec::new(),
                },
            );
            let outcomes: Vec<_> = r.outcomes().collect();
            assert_eq!(outcomes.len(), 1, "{stage:?} decided once");
            assert_eq!(outcomes[0].0, crate::ladder::Outcome::Skip, "{stage:?}");
            assert!(
                outcomes[0]
                    .1
                    .contains("(change-scoped run selected no crates)"),
                "{stage:?} said: {}",
                outcomes[0].1
            );
        }
    }

    #[test]
    fn the_lint_is_denied_warnings_after_the_separator() {
        let a = tippy_args(&Scope::workspace());
        let sep = a.iter().position(|x| x == "--").expect("a -- separator");
        assert_eq!(&a[sep + 1..], ["-D", "warnings"]);
        assert!(a[..sep].contains(&"--all-targets".to_string()));
        // `--keep-going` is a COVERAGE guarantee, not a preference: without it
        // the first crate that trips `-D warnings` ends the run and the rest of
        // the workspace is never linted at all, while the verdict still reads
        // like a statement about the whole tree. It must sit on cargo's side of
        // the separator — passed after `--` it would reach the lint driver,
        // which does not know the flag.
        assert!(
            a[..sep].contains(&"--keep-going".to_string()),
            "the lint must not stop at the first failing crate: {a:?}"
        );
        // …and the `--full` required-features pass holds to the same three.
        let g = tippy_gated_args(&Scope::workspace()).expect("the workspace selects both");
        let sep = g.iter().position(|x| x == "--").expect("a -- separator");
        assert_eq!(&g[sep + 1..], ["-D", "warnings"], "{g:?}");
        for flag in ["--all-targets", "--keep-going"] {
            assert!(g[..sep].contains(&flag.to_string()), "{flag}: {g:?}");
        }
    }

    /// THE MERGE CONTRACT'S CARGO CHILDREN, SPELLED OUT (2026-09-27; a
    /// recorded multiset of every spawned argv held this until then). Each word
    /// is load-bearing, and the regression this catches is a silent narrowing:
    /// an added `--lib`, `--bins` or `--exclude`, a dropped `--no-fail-fast`
    /// or `--keep-going`, would still compile and still print a green row —
    /// over less of the tree than the row's name says.
    #[test]
    fn the_workspace_argvs_are_exactly_these() {
        let ws = Scope::workspace();
        let words = |a: &[&str]| a.iter().map(|w| (*w).to_string()).collect::<Vec<_>>();
        // The compile also asks for cargo's JSON (2026-09-26), which names each
        // test executable for the test run's recording (`crate::testrun`); the
        // message format is not part of cargo's fingerprint, so it builds
        // exactly what it built before.
        assert_eq!(
            test_compile_args(&ws),
            words(&[
                "--unverified",
                "test",
                "--workspace",
                "--no-fail-fast",
                "--no-run",
                "--message-format=json-render-diagnostics"
            ])
        );
        assert_eq!(
            test_run_args(&ws),
            words(&[
                "--unverified",
                "test",
                "--workspace",
                "--no-fail-fast",
                "--tests",
                "--",
                "--skip",
                "measuring::",
                "--skip",
                "launchd_copy_tests::",
            ])
        );
        assert_eq!(
            doctest_args(&ws),
            words(&[
                "--unverified",
                "test",
                "--doc",
                "--workspace",
                "--no-fail-fast"
            ])
        );
        assert_eq!(
            deadline_args(&ws),
            words(&[
                "--unverified",
                "test",
                "--workspace",
                "--no-fail-fast",
                "--lib",
                "--",
                "launchd_copy_tests::",
            ])
        );
        assert_eq!(
            measuring_args(&ws),
            words(&[
                "--unverified",
                "test",
                "--workspace",
                "--no-fail-fast",
                "--test",
                "paint",
                "--test",
                "spin",
                "--",
                "measuring::",
            ])
        );
        assert_eq!(
            tippy_args(&ws),
            words(&[
                "--workspace",
                "--all-targets",
                "--keep-going",
                "--",
                "-D",
                "warnings"
            ])
        );
        assert_eq!(
            tippy_gated_args(&ws),
            Some(words(&[
                "-p",
                "aterm-gui",
                "-p",
                "aterm-scrollback",
                "--features",
                "aterm-gui/bench-support,aterm-gui/control-conformance,aterm-scrollback/disk-tier",
                "--all-targets",
                "--keep-going",
                "--",
                "-D",
                "warnings",
            ]))
        );
    }

    #[test]
    fn tippy_keeps_its_own_target_dir_and_migration_quiet() {
        // Both are load-bearing: a shared target dir would churn the stock build
        // on every gate run, and the migration warning is noise the lint would
        // otherwise turn into a failure under -D warnings.
        let mut c = ctx(Scope::workspace());
        c.tools.tippy = Some(PathBuf::from("/s2/targo-tippy"));
        c.path_env = std::ffi::OsString::from("/usr/bin");
        // Assert on the command the STAGE builds, not on one this test spells
        // out again: a replica agrees with itself even after the stage stops
        // setting a variable.
        let cmd = tippy_cmd(
            &c,
            std::path::Path::new("/s2/targo-tippy"),
            tippy_args(&c.scope),
        );
        let names: Vec<String> = cmd
            .envs
            .iter()
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["PATH", "CARGO_TARGET_DIR", "TRUST_NO_MIGRATE_WARN"]);
        assert_eq!(
            cmd.envs[1].1.to_string_lossy(),
            "/repo/target-tippy",
            "the lint must never share the stock build's target dir"
        );
        assert_eq!(
            cmd.envs[0].1.to_string_lossy(),
            "/s2:/usr/bin",
            "the tippy binary's own dir must LEAD the inherited PATH, so the \
             lint runs under THE toolchain rather than whatever `cargo` the \
             ambient PATH offers first"
        );
    }

    /// Pins which children yield the CPU (see [`Cmd::demoted`]). Every child
    /// that only COMPILES is demoted. Every child that RUNS code keeps the
    /// inherited tier, because a clamp reaches everything it launches, and the
    /// test run and the sealed rung launch aterms.
    #[test]
    fn compile_only_children_yield_and_children_that_run_code_do_not() {
        let mut c = ctx(Scope::workspace());
        c.tools.tippy = Some(PathBuf::from("/s2/targo-tippy"));

        for bind in [false, true] {
            let [compile, run] = test_cmds(&c, bind);
            assert!(compile.demoted, "targo test --no-run");
            assert!(!run.demoted, "targo test --tests runs code");
            assert_eq!(compile.argv()[1..], test_compile_args(&c.scope)[..]);
            assert_eq!(run.argv()[1..], test_run_args(&c.scope)[..]);
            let (_, measuring) = measuring_cmd(&c, bind);
            assert!(
                !measuring.demoted,
                "the measuring tests run the paint and spin guards"
            );
            assert_eq!(measuring.argv()[1..], measuring_args(&c.scope)[..]);
            let (_, deadline) = deadline_cmd(&c, bind);
            assert!(
                !deadline.demoted,
                "the deadline tests time real launchd work"
            );
            assert_eq!(deadline.argv()[1..], deadline_args(&c.scope)[..]);
        }
        assert!(
            conformance_release_cmd(&c).demoted,
            "the release artifact is only compiled here"
        );
        assert!(
            tippy_cmd(
                &c,
                std::path::Path::new("/s2/targo-tippy"),
                tippy_args(&c.scope)
            )
            .demoted,
            "tippy"
        );
        for (label, cmd) in driver_build_cmds(&c) {
            assert!(cmd.demoted, "{label}");
        }
        let [(build_label, build), (suite_label, suite)] = sealed_lane_cmds(&c);
        assert!(build.demoted, "{build_label}");
        assert!(
            !suite.demoted,
            "{suite_label} boots real aterm-gui --headless processes"
        );
        assert!(
            atpkg_build_cmd(&c).demoted,
            "the atpkg the pack suite drives is only compiled here"
        );
    }

    /// THE REACH GUARD FOR THE REACH GUARDS (ported from xtask 2026-09-27, as
    /// a SUBSET check). `--all-targets` builds no target whose
    /// `required-features` are off, so every `(package, feature)` a
    /// `crates/*/Cargo.toml` `required-features` names must be in
    /// [`GATED_LINT_FEATURES`] or that target is linted by nobody — and any
    /// count gate or reach guard living in it stops existing silently (the
    /// four-day bench break). An entry no manifest names any more is not this
    /// test's to find: the `--full` lint fails on a feature cargo does not know.
    #[test]
    fn the_gated_feature_table_covers_every_required_features_target() {
        let crates = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut found: Vec<(String, String)> = Vec::new();
        for entry in std::fs::read_dir(&crates)
            .expect("crates/ is readable")
            .flatten()
        {
            let Ok(text) = std::fs::read_to_string(entry.path().join("Cargo.toml")) else {
                continue;
            };
            // The PACKAGE name, from the `[package]` table: `-p` takes it, and
            // neither the directory nor a `[[bin]]`'s `name =` has to equal it.
            let Some(pkg) = text
                .lines()
                .map(str::trim)
                .skip_while(|l| *l != "[package]")
                .find_map(|l| l.strip_prefix("name = "))
                .map(|n| n.trim().trim_matches('"').to_string())
            else {
                continue;
            };
            // The DECLARATION only, never the prose beside it.
            for list in text.lines().filter_map(|l| {
                l.trim()
                    .strip_prefix("required-features")
                    .and_then(|r| r.trim_start().strip_prefix('='))
            }) {
                for feat in list.trim().trim_matches(['[', ']']).split(',') {
                    let feat = feat.trim().trim_matches('"');
                    if !feat.is_empty() {
                        found.push((pkg.clone(), feat.to_string()));
                    }
                }
            }
        }
        assert!(
            !found.is_empty(),
            "the manifest scan found nothing — it broke"
        );
        for (pkg, feat) in &found {
            assert!(
                GATED_LINT_FEATURES.contains(&(pkg.as_str(), feat.as_str())),
                "{pkg}/{feat} gates a target that GATED_LINT_FEATURES does not name, so \
                 no lint pass builds it: {found:?}"
            );
        }
    }

    #[test]
    fn the_gated_pass_narrows_with_the_scope_and_disappears_when_it_has_nothing_to_reach() {
        // One gated package selected: only its features, only its `-p`.
        let one = tippy_gated_args(&Scope::crate_only("aterm-scrollback"))
            .expect("aterm-scrollback owns two gated benches");
        assert_eq!(
            one,
            [
                "-p",
                "aterm-scrollback",
                "--features",
                "aterm-scrollback/disk-tier",
                "--all-targets",
                "--keep-going",
                "--",
                "-D",
                "warnings",
            ]
        );
        // A scope with no gated target must not compile two crates it was
        // narrowed away from just to lint nothing.
        assert_eq!(tippy_gated_args(&Scope::crate_only("aterm-core")), None);
        assert_eq!(
            tippy_gated_args(&Scope::changed("main", vec!["aterm-core".into()], true)),
            None
        );
        assert!(
            tippy_gated_args(&Scope::changed("main", vec!["aterm-gui".into()], true)).is_some(),
            "a changed-scope run that rebuilt aterm-gui must still lint its benches"
        );
    }

    /// The environment variable names one command carries, in order.
    fn env_names(cmd: &Cmd) -> Vec<String> {
        cmd.envs
            .iter()
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn the_test_run_runs_the_trail_laws_full_grid() {
        // Without it the trail laws run only each family's spine in the
        // gate's debug build, and a LIMIT rule the full grid breaks is seen
        // only by a release run by hand.
        let c = ctx(Scope::workspace());
        for bind in [false, true] {
            let [compile, run] = test_cmds(&c, bind);
            let (k, v) = TRAIL_LAWS_FULL;
            assert!(
                run.envs
                    .iter()
                    .any(|(ek, ev)| ek.to_string_lossy() == k && ev.to_string_lossy() == v),
                "the test run carries {k}={v}: {:?}",
                run.envs
            );
            assert!(
                !env_names(&compile).iter().any(|n| n == k),
                "the compile needs no copy of {k}"
            );
        }
    }

    #[test]
    fn the_doc_running_stages_bind_trusts_renamed_doc_driver() {
        // Trust renames rustdoc to `trustdoc`. Unbound, doctests either fail to
        // launch or run under whatever rustdoc the ambient PATH offers — which
        // is not the driver the rest of the gate used.
        let c = ctx(Scope::workspace());
        let cmd = with_trustdoc(&c, targo(&c, doctest_args(&c.scope)));
        assert_eq!(env_names(&cmd), ["RUSTDOC"]);
        assert_eq!(
            cmd.envs[0].1.as_os_str(),
            c.tools.trustdoc.as_os_str(),
            "the doc driver must be THE toolchain's, by absolute path"
        );
    }

    #[test]
    fn every_kani_crate_gets_its_own_selector() {
        // One script, one run per crate, distinguished ONLY by `KANI_CRATE`.
        // Drop it and every iteration proves the same crate while the ladder
        // still prints a green row each — N claims, one of them true.
        let gate = std::path::Path::new("/repo/tools/verify-kani-proofs.sh");
        let mc = std::path::Path::new("/store/trust-mc/current");
        let ay = std::path::Path::new("/store/bin");
        let selectors: Vec<String> = KANI_CRATES
            .iter()
            .map(|k| {
                let cmd = kani_cmd(gate, k, mc, ay);
                assert_eq!(
                    env_names(&cmd),
                    ["KANI_CRATE", "TRUST_MC_SYSROOT", "AY_BIN_DIR"]
                );
                assert_eq!(
                    cmd.envs[1].1.as_os_str(),
                    mc.as_os_str(),
                    "the script drives THIS trust-mc"
                );
                assert_eq!(cmd.envs[2].1.as_os_str(), ay.as_os_str(), "and THIS ay");
                cmd.envs[0].1.to_string_lossy().into_owned()
            })
            .collect();
        assert_eq!(selectors, KANI_CRATES);
        assert_eq!(
            selectors
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            KANI_CRATES.len(),
            "each run must select a DIFFERENT crate"
        );
    }

    #[test]
    fn trust_mc_and_ay_resolve_env_then_store_and_never_a_build_tree() {
        // The order the script uses, decided in one place so the availability
        // check and the run can never disagree: explicit env wins outright, else
        // the store — even with a from-source checkout sitting under home.
        let home = crate::mktemp_dir("atv-kani").expect("mktemp");
        let prefix = crate::toolchain::default_atpkg_prefix(&home);
        for dev in [
            "trust/first-party/trust-mc/target/trust-mc/bin",
            "trust/first-party/ay/target/release",
        ] {
            std::fs::create_dir_all(home.join(dev)).expect("mkdir");
        }
        let env = EnvSnapshot {
            home: home.clone(),
            ..EnvSnapshot::default()
        };
        assert_eq!(
            trust_mc_sysroot(&env),
            prefix.join("store/trust-mc/current")
        );
        assert_eq!(ay_bin_dir(&env), prefix.join("bin"));
        // A store that is there answers its BUILD-NUMBERED directory, which no
        // `aterm pkg update` can re-point under the three runs of the floor.
        #[cfg(unix)]
        {
            std::fs::create_dir_all(prefix.join("store/trust-mc/41/bin")).expect("mkdir");
            std::os::unix::fs::symlink("41", prefix.join("store/trust-mc/current")).expect("ln");
            assert_eq!(
                trust_mc_sysroot(&env),
                std::fs::canonicalize(prefix.join("store/trust-mc/41")).expect("real"),
                "the live link is resolved once, not handed to the script as `current`"
            );
        }

        let env = EnvSnapshot {
            trust_mc_sysroot: Some(PathBuf::from("/explicit/sysroot")),
            ay_bin_dir: Some(PathBuf::from("/explicit/ay")),
            ..env
        };
        assert_eq!(trust_mc_sysroot(&env), PathBuf::from("/explicit/sysroot"));
        assert_eq!(ay_bin_dir(&env), PathBuf::from("/explicit/ay"));
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn no_targo_is_fail_closed_at_the_test_compile_and_honest_everywhere_after() {
        let c = ctx(Scope::workspace());
        assert!(!c.tools.have_targo());

        let mut r = Report::new("test compile");
        test_compile(&c, &mut r);
        let (outcome, label) = r.outcomes().next().expect("a decision");
        assert_eq!(outcome, crate::Outcome::Fail(crate::Severity::CouldNotRun));
        assert!(label.starts_with("targo not found"), "{label}");
        assert!(
            label.contains("x.py build --stage 2"),
            "the diagnostic says how to fix it"
        );

        // The dependent stages then skip — honestly, and named, so the verdict
        // refuses the merge contract for the whole run.
        let dependent: [fn(&Ctx, &mut Report); 7] = [
            test,
            deadline_tests,
            measuring_tests,
            doctests,
            freeze_gate,
            driver_builds,
            redraw_conformance,
        ];
        for stage in dependent {
            let mut r = Report::new("s");
            stage(&c, &mut r);
            for (outcome, label) in r.outcomes() {
                assert_eq!(outcome, crate::Outcome::Skip, "{label}");
                assert!(label.contains("no targo"), "{label}");
            }
        }
    }

    /// AN UNAVAILABLE PROVER IS SAID PROMINENTLY AND NEVER DESCRIBED AS
    /// DISCHARGED (`--full`'s trust-mc / Kani floor): a NOTICE naming what did
    /// not run and its repair, one named skip, and so no merge-contract claim.
    #[cfg(unix)]
    #[test]
    fn an_unavailable_prover_is_a_named_skip_that_forfeits_the_claim() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = crate::mktemp_dir("atv-kani-absent").expect("mktemp");
        let script = tmp.join("scripts/verify-kani-proofs.sh");
        std::fs::create_dir_all(tmp.join("scripts")).expect("mkdir");
        std::fs::write(&script, "#!/bin/sh\nexit 0\n").expect("write");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let mut c = ctx(Scope::workspace());
        c.root = tmp.clone();
        c.env.trust_mc_sysroot = Some(tmp.join("no-trust-mc"));
        c.env.ay_bin_dir = Some(tmp.join("no-ay"));
        let r = run_stage(
            &c,
            &StageSpec {
                id: StageId::KaniFloor,
                title: "trust-mc / Kani BMC floor".into(),
                lane: Lane::MainTarget,
                exclusive: false,
                after_lanes: Vec::new(),
            },
        );
        let text = r.render();
        assert!(
            text.contains(
                "  NOTICE: Tier-2 trust-mc/Kani obligations were NOT RUN: trust-mc is unavailable"
            ),
            "{text}"
        );
        assert!(
            text.contains(
                "  skip  trust-mc / Kani BMC floor (tool unavailable; `aterm pkg install trust-mc`)"
            ),
            "the skip names the repair: {text}"
        );
        assert!(
            !text.contains("verify-kani-proofs.sh (aterm-parser)"),
            "never described as discharged: {text}"
        );
        let t = crate::ladder::tally(std::slice::from_ref(&r));
        assert_eq!(t.skipped(), 1, "{text}");
        assert!(!crate::verdict::discharges_merge_contract(
            crate::Mode::Full,
            &Scope::workspace(),
            &t
        ));
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn a_missing_helper_script_can_never_pass() {
        let c = ctx(Scope::workspace());
        let script_stages: [fn(&Ctx, &mut Report); 5] = [
            grep_guards,
            delivery_tooling,
            atpkg_tooling,
            start_compare,
            libc_oracle,
        ];
        for stage in script_stages {
            let mut r = Report::new("s");
            stage(&c, &mut r);
            let (outcome, label) = r.outcomes().next().expect("a decision");
            assert_eq!(
                outcome,
                crate::Outcome::Fail(crate::Severity::CouldNotRun),
                "{label} must fail closed"
            );
            assert!(label.contains("missing or not executable"), "{label}");
        }
    }

    /// Each delivery suite is a decision of its own, run from the tree the gate
    /// verifies: a red suite fails the stage under its own name while its
    /// siblings still run, and a missing one is a cannot-run. The first pass,
    /// over every suite green, is the negative control: the stage decides one
    /// `ok` per suite, so the red and missing rows below are the suites' doing.
    #[cfg(unix)]
    #[test]
    fn every_delivery_suite_runs_and_decides_on_its_own() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = crate::mktemp_dir("atv-delivery-suites").expect("mktemp");
        let tools = tmp.join("tools");
        std::fs::create_dir_all(&tools).expect("mkdir");
        let suite = |name: &str, body: &str| {
            let path = tools.join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        };
        for name in DELIVERY_SUITES {
            suite(name, "exit 0");
        }
        let mut c = ctx(Scope::workspace());
        c.root = tmp.clone();
        let decide = |c: &Ctx| {
            let mut r = Report::new("delivery tooling");
            delivery_tooling(c, &mut r);
            r.outcomes()
                .map(|(o, l)| (o, l.to_string()))
                .collect::<Vec<_>>()
        };

        let green = decide(&c);
        assert_eq!(
            green,
            DELIVERY_SUITES
                .iter()
                .map(|n| (Outcome::Ok, (*n).to_string()))
                .collect::<Vec<_>>()
        );
        // Each is its own suite, and a killed one is sent SIGTERM first so its
        // EXIT trap cleans up (dev-sign-id's keychain is the reason).
        // …and each is handed NOTHING: the atpkg suites among them measure
        // producer scripts with `$ATPKG` unset, which an exported binary would
        // stop them measuring.
        for name in DELIVERY_SUITES {
            let cmd = delivery_suite_cmd(&c, name);
            assert_eq!(cmd.argv(), [tools.join(name).display().to_string()]);
            assert_eq!(cmd.term_grace, Some(exec::TERM_GRACE), "{name}");
            assert!(cmd.envs.is_empty(), "{name}: {:?}", cmd.envs);
        }

        suite("test-publish-export.sh", "echo widened >&2; exit 1");
        std::fs::remove_file(tools.join("test-release-preflight.sh")).expect("rm");
        let mixed = decide(&c);
        assert_eq!(mixed.len(), DELIVERY_SUITES.len(), "{mixed:?}");
        assert_eq!(
            mixed[0],
            (Outcome::Ok, "test-install-channel.sh".to_string())
        );
        assert_eq!(
            mixed[1],
            (
                Outcome::Fail(Severity::GateFailed),
                "test-publish-export.sh".to_string()
            )
        );
        assert_eq!(mixed[2].0, Outcome::Fail(Severity::CouldNotRun));
        assert!(
            mixed[2]
                .1
                .starts_with("test-release-preflight.sh missing or not executable"),
            "{mixed:?}"
        );
        // …and every suite after them still ran, the five that joined on
        // 2026-09-26 included: one red row never hides the rest of the roster.
        assert!(
            mixed[3..].iter().all(|(o, _)| *o == Outcome::Ok),
            "{mixed:?}"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn the_redraw_gate_builds_the_feature_without_which_it_builds_nothing() {
        // `aterm-redraw-conformance` carries `required-features =
        // ["control-conformance"]`. Drop the feature and cargo does not error —
        // it matches no target and exits 0, so the stage would sail on and
        // decide on whatever binary an earlier run happened to leave behind.
        let a = redraw_conformance_build_args();
        let feat = a
            .iter()
            .position(|x| x == "--features")
            .expect("the gate must ask for the feature that compiles it");
        assert_eq!(a[feat + 1], "control-conformance");
        let bin = a.iter().position(|x| x == "--bin").expect("a --bin");
        assert_eq!(a[bin + 1], REDRAW_CONFORMANCE_BIN);
        assert!(a.contains(&"aterm-gui".to_string()));
    }

    #[test]
    fn a_cell_gate_that_compiled_nothing_is_counted_as_a_skip_not_a_pass() {
        // THE WHOLE POINT OF READING THE TRANSCRIPT. `gate cells` exits 0 for an
        // uninstalled std on purpose, so the exit code cannot tell the matrix
        // discharged from no compiler having started. The words can.
        let unproven = format!(
            "{CELLS_NOT_PROVEN} — 5 of the 5 cell(s) this run selected had no installed std, \
             so NO COMPILER READ THEM: mac-arm (aarch64-apple-darwin), …\n"
        );
        let (outcome, label) = cells_outcome("gate cells", true, &unproven);
        assert_eq!(outcome, Outcome::Skip);
        assert_ne!(outcome, Outcome::Ok);
        assert!(label.contains("NOTHING was compiled"), "{label}");

        // A run that really did discharge it is still a pass.
        let green = "gate cells: GREEN — all 5 cells type-check, with nothing excused or \
                     shimmed: every one of the 312 in-repo crate-instances …\n";
        assert_eq!(cells_outcome("gate cells", true, green).0, Outcome::Ok);

        // A failure stays a failure however the transcript reads — including one
        // that also carries the marker, which a partly-skipped red run does.
        assert_eq!(
            cells_outcome("gate cells", false, &unproven).0,
            Outcome::Fail(Severity::GateFailed)
        );
        assert_eq!(
            cells_outcome("gate cells", false, green).0,
            Outcome::Fail(Severity::GateFailed)
        );

        // ANCHORED AT COLUMN 0: a compiler diagnostic that quotes the sentinel
        // out of this repo's own source is tree text, not a verdict.
        let quoted = format!(
            "   |     pub const CELLS_NOT_PROVEN: &str = \"{CELLS_NOT_PROVEN}\";\n\
             gate cells: GREEN — all 5 cells type-check…\n"
        );
        assert_eq!(cells_outcome("gate cells", true, &quoted).0, Outcome::Ok);
    }

    #[test]
    fn a_skipped_cell_gate_cannot_leave_the_merge_contract_claimed() {
        // The property the mapping exists for, asserted end to end against the
        // verdict: one skipped stage, whole-tree scope, nothing failed — and the
        // sentence is still withheld.
        let mut r = Report::new("cross-cell type-check");
        let (outcome, label) = cells_outcome(
            "gate cells-foreign",
            true,
            &format!("{CELLS_NOT_PROVEN} — 5 of 5 …\n"),
        );
        r.record(outcome, label);
        let t = crate::ladder::tally(&[r]);
        assert_eq!(t.skipped(), 1);
        assert!(!crate::verdict::discharges_merge_contract(
            crate::Mode::Full,
            &crate::scope::Scope::workspace(),
            &t
        ));
    }

    #[test]
    fn the_objc_audits_four_codes_map_to_four_distinct_claims() {
        // EXIT 2 IS NEVER GREEN. It is the auditor saying a row had no authority
        // at all, or that no window server answered — the silence this gate was
        // written to remove.
        let (two, two_label) = objc_audit_outcome(Some(2));
        assert_eq!(two, Outcome::Fail(Severity::CouldNotRun));
        assert_ne!(two, Outcome::Ok);
        assert_ne!(two, Outcome::Skip);
        assert!(two_label.contains("NOT RUN"), "{two_label}");

        // EXIT 3 IS GREEN, AND SAYS A WEAKER THING THAN 0. It is the pass a host
        // whose AppKit lacks a claimed protocol can reach: every row held to a
        // shape, some to the fork's own declaration rather than the runtime's. A
        // gate that read this as `0` would write a receipt asserting runtime
        // authority for rows the runtime could not speak for.
        let (three, three_label) = objc_audit_outcome(Some(3));
        assert_eq!(three, Outcome::Ok);
        assert!(
            three_label.contains("FORK'S OWN DECLARATION"),
            "exit 3's label must name the authority it actually used: {three_label}"
        );
        assert_ne!(
            three_label,
            objc_audit_outcome(Some(0)).1,
            "0 and 3 are different claims and may not share a sentence"
        );
        assert!(
            objc_audit_outcome(Some(0))
                .1
                .contains("runtime's own authority"),
            "exit 0 must keep claiming the runtime arbitrated"
        );
        assert!(
            !objc_audit_outcome(Some(0)).1.contains("FORK'S OWN"),
            "exit 0 must not claim the fork-declaration path"
        );

        assert_eq!(objc_audit_outcome(Some(0)).0, Outcome::Ok);
        assert_eq!(
            objc_audit_outcome(Some(1)).0,
            Outcome::Fail(Severity::GateFailed)
        );
        // Anything else is a broken auditor, never green: a panic (101), a code
        // a future edit adds without teaching this function, and no status at
        // all (a signal).
        for code in [4, 5, 101, 255] {
            assert_eq!(
                objc_audit_outcome(Some(code)).0,
                Outcome::Fail(Severity::GateFailed),
                "exit {code} must not be green"
            );
        }
        assert_eq!(
            objc_audit_outcome(None).0,
            Outcome::Fail(Severity::CouldNotRun)
        );
    }

    #[test]
    fn a_redraw_gate_that_could_not_run_is_never_a_pass_and_never_a_skip() {
        // THE WHOLE POINT OF THE MAPPING. Exit 2 is the harness saying no event
        // loop is constructible here; a headless box must not read that as green,
        // and it must not read as a quiet skip either — that is the same false
        // green one layer up.
        let (outcome, label) = redraw_outcome(Some(2));
        assert_eq!(outcome, Outcome::Fail(Severity::CouldNotRun));
        assert_ne!(outcome, Outcome::Ok);
        assert_ne!(outcome, Outcome::Skip);
        assert!(label.contains("NOT RUN"), "{label}");

        assert_eq!(redraw_outcome(Some(0)).0, Outcome::Ok);
        assert_eq!(
            redraw_outcome(Some(1)).0,
            Outcome::Fail(Severity::GateFailed)
        );
        // A panic (101) or a signal is a finding/that-decided-nothing, never green.
        assert_eq!(
            redraw_outcome(Some(101)).0,
            Outcome::Fail(Severity::GateFailed)
        );
        assert_eq!(redraw_outcome(None).0, Outcome::Fail(Severity::CouldNotRun));
        for code in [None, Some(1), Some(2), Some(101), Some(-1)] {
            assert_ne!(redraw_outcome(code).0, Outcome::Ok, "{code:?}");
        }
    }

    /// THE TEST RUN AND THE TWO FILTERED STAGES SPLIT ONE SELECTION (2026-09-23,
    /// split in two on 2026-09-26, narrowed to their targets on 2026-09-27):
    /// the same PACKAGE selection in every scope, the run skipping each filter
    /// of both lists and each stage running exactly its own list over the
    /// targets that hold it — which [`every_skipped_module_is_declared_where_its_stage_looks`]
    /// makes the only targets a filter can match — and the lists are disjoint,
    /// so no test is owned by both tiers. The negative control is the
    /// pre-split argv, which skipped nothing and ran every one of them in the
    /// parallel run.
    #[test]
    fn the_test_run_and_the_filtered_stages_split_one_selection_by_their_filter_lists() {
        fn strs(a: &[String]) -> Vec<&str> {
            a.iter().map(String::as_str).collect()
        }
        for s in [
            Scope::workspace(),
            Scope::crate_only("aterm-conformance"),
            Scope::changed("main", vec!["atpkg".into(), "aterm-update".into()], true),
        ] {
            let base = test_base_args(&s);
            let run = test_run_args(&s);
            let split = |a: &[String]| {
                let at = a.iter().position(|x| x == "--").expect("libtest args");
                (a[..at].to_vec(), a[at + 1..].to_vec())
            };
            let (run_cargo, run_libtest) = split(&run);
            assert_eq!(run_cargo[..base.len()], base[..], "{run:?}");
            assert_eq!(strs(&run_cargo[base.len()..]), ["--tests"], "{run:?}");
            let skipped: Vec<&str> = run_libtest
                .chunks(2)
                .map(|pair| {
                    assert_eq!(pair[0], "--skip", "{run_libtest:?}");
                    pair[1].as_str()
                })
                .collect();
            let mut both: Vec<&str> = Vec::new();
            for (args, targets, list) in [
                (
                    measuring_args(&s),
                    &MEASURING_TARGETS[..],
                    &MEASURING_TESTS[..],
                ),
                (
                    deadline_args(&s),
                    &DEADLINE_TARGETS[..],
                    &DEADLINE_TESTS[..],
                ),
            ] {
                let (cargo, libtest) = split(&args);
                assert_eq!(cargo[..base.len()], base[..], "one selection: {args:?}");
                assert_eq!(strs(&cargo[base.len()..]), targets, "{args:?}");
                assert_eq!(strs(&libtest), list, "{args:?}");
                both.extend(list);
            }
            assert_eq!(skipped, both, "{run:?}");
        }
        assert!(
            MEASURING_TESTS.iter().all(|m| !DEADLINE_TESTS.contains(m)),
            "one tier owns each filter"
        );
        // The negative control: the argv before the split skipped nothing, so
        // every one of these ran in the parallel run.
        let before = test_base_args(&Scope::workspace());
        assert!(!before.iter().any(|a| a == "--skip"), "{before:?}");
        // …and every filter names a module, so it can only match a test path
        // (`measuring::row`), never a bare function name that happens to
        // contain the word.
        for f in MEASURING_TESTS.iter().chain(&DEADLINE_TESTS) {
            assert!(f.ends_with("::"), "{f}");
        }
    }

    /// The modules `src` declares: every `mod <name>` that opens an item — at
    /// the start of a line, or after a visibility, an attribute or another
    /// item on it — outside `//` comments. Text that merely reads `mod x`
    /// inside a string is not one: its quote precedes it.
    fn declared_modules(src: &str) -> Vec<&str> {
        let ident = |c: char| c.is_alphanumeric() || c == '_';
        let mut found = Vec::new();
        for line in src.lines().map(str::trim_start) {
            if line.starts_with("//") {
                continue;
            }
            let mut from = 0;
            while let Some(at) = line[from..].find("mod ").map(|i| from + i) {
                from = at + 4;
                let before = line[..at].trim_end();
                let opens = before.is_empty()
                    || before.ends_with("pub")
                    || before.ends_with([')', ']', '{', '}', ';']);
                if !opens || line[..at].ends_with(ident) {
                    continue;
                }
                let rest = line[from..].trim_start();
                let name = &rest[..rest.find(|c| !ident(c)).unwrap_or(rest.len())];
                if !name.is_empty() {
                    found.push(name);
                }
            }
        }
        found
    }

    /// A SKIPPED MODULE IS DECLARED ONLY WHERE ITS STAGE LOOKS (2026-09-27).
    ///
    /// libtest's `--skip` is a SUBSTRING match, so the test run's `--skip
    /// measuring::` skips every test under any module whose name ENDS in
    /// `measuring` (`spin_measuring::row` too), in every target of every crate
    /// — while the measuring stage selects only the `paint` and `spin` targets
    /// and the deadline stage only libraries. A module named to match a filter
    /// anywhere else would be skipped by the run and selected by no stage:
    /// tests that never run and never say so. So, read from every source under
    /// `crates/` (the workspace's members): a module whose name ends in a
    /// filter's is declared in one of that filter's own sites. And each site
    /// declares its module, so the walk is not vacuous and the stage has
    /// something to select.
    #[test]
    fn every_skipped_module_is_declared_where_its_stage_looks() {
        let owners: [(&str, &[&str]); 2] = [
            (
                "measuring::",
                &[
                    "aterm-conformance/tests/paint.rs",
                    "aterm-conformance/tests/spin.rs",
                ],
            ),
            ("launchd_copy_tests::", &["aterm-update/src/install.rs"]),
        ];
        let filters: Vec<&str> = MEASURING_TESTS
            .iter()
            .chain(&DEADLINE_TESTS)
            .copied()
            .collect();
        assert_eq!(owners.map(|(f, _)| f).to_vec(), filters, "a row per filter");
        let owner = |name: &str| {
            owners
                .iter()
                .find(|(f, _)| name.ends_with(f.trim_end_matches(':')))
        };

        // The parser, on the shapes that matter: it must see these…
        // (spelled through `m`, so this file declares nothing the walk finds)
        let m = "mod";
        let seen = format!(
            "{m} measuring;\n#[cfg(test)] {m} spin_measuring {{\npub(crate) {m} launchd_copy_tests;\n"
        );
        assert_eq!(
            declared_modules(&seen),
            ["measuring", "spin_measuring", "launchd_copy_tests"]
        );
        // …and none of these.
        let unseen = format!("// {m} measuring;\nlet s = \"{m} measuring;\";\nre{m} x\n");
        assert!(declared_modules(&unseen).is_empty(), "{unseen}");

        let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut found: Vec<(String, String)> = Vec::new();
        let mut stack = vec![crates.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("a source dir") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    if path.file_name().is_some_and(|n| n != "target") {
                        stack.push(path);
                    }
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let text = std::fs::read_to_string(&path).expect("a source file");
                    let rel = path
                        .strip_prefix(&crates)
                        .expect("under crates/")
                        .components()
                        .map(|c| c.as_os_str().to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                        .join("/");
                    for name in declared_modules(&text) {
                        if owner(name).is_some() {
                            found.push((rel.clone(), name.to_string()));
                        }
                    }
                }
            }
        }
        for (rel, name) in &found {
            let (filter, sites) = owner(name).expect("matched above");
            assert!(
                sites.contains(&rel.as_str()),
                "`mod {name}` in crates/{rel} matches the test run's `--skip {filter}`, but \
                 the stage that runs that filter looks only in {sites:?}, so its tests would \
                 run nowhere: rename the module, or move it into one of those targets"
            );
        }
        found.sort();
        assert_eq!(
            found,
            [
                ("aterm-conformance/tests/paint.rs", "measuring"),
                ("aterm-conformance/tests/spin.rs", "measuring"),
                ("aterm-update/src/install.rs", "launchd_copy_tests"),
            ]
            .map(|(r, n)| (r.to_string(), n.to_string())),
            "each site declares the module its stage selects"
        );
    }

    /// A caller's `CARGO_BUILD_JOBS` is a CEILING for the side lanes' constants —
    /// never a floor, never a parse error. Measured 2026-09-15 on a 4-core Intel
    /// MacBook Pro: `CARGO_BUILD_JOBS=4 tools/verify.sh --fast` capped the main
    /// and lint lanes (they inherit) while the driver lane went to 8 and the
    /// three side lanes to 4 regardless, so the export changed nothing about
    /// the overlap that ran the box at a load of 17–20.
    #[test]
    fn a_callers_cargo_build_jobs_is_a_ceiling_for_the_side_lanes() {
        use std::ffi::OsStr;
        let xtask = Lane::XtaskTarget;
        assert_eq!(lane_jobs(xtask, None), Some(4));
        assert_eq!(lane_jobs(xtask, Some(OsStr::new("2"))), Some(2));
        assert_eq!(lane_jobs(xtask, Some(OsStr::new(" 3 "))), Some(3));
        assert_eq!(lane_jobs(xtask, Some(OsStr::new("16"))), Some(4));
        assert_eq!(lane_jobs(Lane::DriverTarget, None), Some(8));
        assert_eq!(
            lane_jobs(Lane::DriverTarget, Some(OsStr::new("4"))),
            Some(4)
        );
        assert_eq!(
            lane_jobs(Lane::DriverTarget, Some(OsStr::new("8"))),
            Some(8)
        );
        // Not a positive integer: the constant stands, and the raw value still
        // reaches the main lane by inheritance to fail with cargo's own message.
        for junk in ["", "0", "-1", "four", "4.5"] {
            assert_eq!(
                lane_jobs(Lane::DriverTarget, Some(OsStr::new(junk))),
                Some(8),
                "{junk:?}"
            );
        }
        // Lanes with no cap of their own stay uncapped here — they inherit.
        for lane in [Lane::MainTarget, Lane::Pure, Lane::TippyTarget] {
            assert_eq!(lane_jobs(lane, Some(OsStr::new("2"))), None, "{lane:?}");
        }
        // Through `in_lane`: the ceiling reaches the child's environment.
        let mut c = ctx(Scope::workspace());
        c.env.cargo_build_jobs = Some("3".into());
        let envs = |cmd: &Cmd| -> Vec<(String, String)> {
            cmd.envs
                .iter()
                .map(|(k, v)| {
                    (
                        k.to_string_lossy().into_owned(),
                        v.to_string_lossy().into_owned(),
                    )
                })
                .collect()
        };
        for (label, cmd) in driver_build_cmds(&c) {
            assert!(
                envs(&cmd).contains(&("CARGO_BUILD_JOBS".to_string(), "3".to_string())),
                "{label}: {:?}",
                envs(&cmd)
            );
        }
        assert!(
            envs(&xtask_cmd(&c, xtask_gate_args("forge")))
                .contains(&("CARGO_BUILD_JOBS".to_string(), "3".to_string()))
        );
    }

    #[test]
    fn side_lane_cmds_name_their_own_target_dir() {
        let env = |cmd: &Cmd| -> Vec<(String, String)> {
            cmd.envs
                .iter()
                .map(|(k, v)| {
                    (
                        k.to_string_lossy().into_owned(),
                        v.to_string_lossy().into_owned(),
                    )
                })
                .collect()
        };
        let lane = |dir: &str, jobs: Option<&str>| {
            let mut v = vec![("CARGO_TARGET_DIR".to_string(), dir.to_string())];
            if let Some(j) = jobs {
                v.push(("CARGO_BUILD_JOBS".to_string(), j.to_string()));
            }
            v
        };
        let c = ctx(Scope::workspace());

        for cmd in [
            xtask_cmd(&c, xtask_gate_args_with("lint", &["--fmt-only"])),
            xtask_cmd(&c, xtask_gate_args("forge")),
        ] {
            assert_eq!(env(&cmd), lane("/repo/target-xtask", Some("4")));
        }
        for (label, cmd) in driver_build_cmds(&c)
            .into_iter()
            .chain(sealed_lane_cmds(&c))
            .chain([(ATPKG_BUILD_LABEL.to_string(), atpkg_build_cmd(&c))])
        {
            assert_eq!(
                env(&cmd),
                lane("/repo/target-drivers", Some("8")),
                "{label}"
            );
        }
        for args in [
            redraw_conformance_build_args(),
            objc_class_audit_build_args(),
            objc_bound_drive_build_args(),
        ] {
            assert_eq!(
                env(&driver_build_cmd(&c, args)),
                lane("/repo/target-drivers", Some("8"))
            );
        }
        assert_eq!(drivers_dir(&c), PathBuf::from("/repo/target-drivers"));
        assert_eq!(
            env(&conformance_release_cmd(&c)),
            lane("/repo/target/conformance-release", Some("6")),
            "the prime names the helper's dir, never the caller's"
        );
        assert_eq!(
            env(&freeze_gate_cmd(&c)),
            lane("/repo/tools/freeze-safety-gate/target", None),
            "the L0 gate names the dir cargo used when nothing was exported"
        );
        // The main lane is handed nothing: its children keep inheriting
        // the caller's target dir, exactly as before.
        assert!(
            env(&in_lane(
                &c,
                Lane::MainTarget,
                targo(&c, test_compile_args(&c.scope))
            ))
            .is_empty()
        );
    }

    /// EVERY DRIVEN BINARY IS BUILT IN THE DRIVER LANE AND HANDED TO ITS SUITE
    /// — one law for the four rows whose suites drive a binary their own stage
    /// builds: the sealed rung (2026-09-14), the atpkg pack (2026-09-16) and
    /// the two live lanes (2026-09-26). Whatever the caller exported, for each:
    /// the stage's build child only compiles; it and the suite name ONE dir,
    /// the lane the plan schedules the row in; the binary is that dir's, never
    /// `<root>/target`'s — the suites' own default, and a previous run's or one
    /// still linking; and the suite is handed it in the spelling its own source
    /// reads (read from the tree, so a renamed flag reddens here instead of
    /// falling back to the default). The sealed rung is handed NOTHING beyond
    /// the lane: its harness finds `aterm-gui` in the dir the suite was built
    /// into and refuses a stale one, and `ATERM_GUI_BIN` would skip that
    /// refusal instead of satisfying it.
    #[test]
    fn every_driven_binary_is_built_in_the_driver_lane_and_handed_to_its_suite() {
        enum Handed {
            /// Found by the suite's harness in the dir it was built into.
            Discovered,
            /// In this environment variable, and nothing else is set.
            Env(&'static str),
            /// As these arguments, and no environment at all.
            Argv(Vec<String>),
        }
        struct Row {
            id: StageId,
            build: Cmd,
            build_args: Vec<String>,
            suite: Cmd,
            bin: PathBuf,
            handed: Handed,
            /// The file that reads the handover, and what it must spell.
            source: &'static str,
            reads: &'static [&'static str],
        }
        let lane = |cmd: &Cmd| {
            cmd.envs
                .iter()
                .find(|(k, _)| k.to_str() == Some("CARGO_TARGET_DIR"))
                .map(|(_, v)| PathBuf::from(v))
        };
        let mut c = ctx(Scope::workspace());
        // The sealed rung and the Codex lane are `--full`'s.
        c.mode = Mode::Full;
        let plan = crate::plan::plan(&c);
        let drivers = drivers_dir(&c);
        let aterm = live_aterm_binary(&c);
        let [(_, sealed_build), (_, sealed_suite)] = sealed_lane_cmds(&c);
        let rows = [
            Row {
                id: StageId::SealedLane,
                build: sealed_build,
                build_args: smoke_build_args(),
                suite: sealed_suite,
                bin: driver_bin(&c, "aterm-gui"),
                handed: Handed::Discovered,
                source: "crates/aterm-link/tests/harness/mod.rs",
                reads: &[r#"built_binary("aterm-gui", "aterm-gui", "ATERM_GUI_BIN")"#],
            },
            Row {
                id: StageId::AtpkgTooling,
                build: atpkg_build_cmd(&c),
                build_args: atpkg_build_args(),
                suite: atpkg_pack_cmd(&c),
                bin: atpkg_driven_binary(&c),
                handed: Handed::Env("ATPKG"),
                source: "tools/test-atpkg-pack-one-compiler.sh",
                reads: &[r#"ATPKG_BIN="${ATPKG:-}""#],
            },
            Row {
                id: StageId::ForegroundHandback,
                build: live_aterm_build_cmd(&c),
                build_args: live_aterm_build_args(),
                suite: live_aterm_suite_cmd(&c, FOREGROUND_HANDBACK_SUITE),
                bin: aterm.clone(),
                handed: Handed::Argv(vec!["--binary".into(), aterm.display().to_string()]),
                source: "tools/test-foreground-handback.sh",
                // …its flag, its not-run code, which only `not_run` spells,
                // and the teardown that turns any other 2 into a finding.
                reads: &[
                    "--binary) BIN=$2; shift 2 ;;",
                    "    NOT_RUN=1\n    exit 2\n}",
                    "if [[ $status -eq 2 && -z $NOT_RUN ]]; then\n        status=1",
                ],
            },
            Row {
                id: StageId::CodexLiveUpgrade,
                build: live_aterm_build_cmd(&c),
                build_args: live_aterm_build_args(),
                suite: live_aterm_suite_cmd(&c, CODEX_LIVE_UPGRADE_SUITE),
                bin: aterm.clone(),
                handed: Handed::Argv(vec![aterm.display().to_string()]),
                source: "tools/test-codex-live-upgrade.sh",
                reads: &[r#"A="${1:-$ROOT/target/debug/aterm}""#, "exit 77; }"],
            },
        ];
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for row in rows {
            let what = format!("{:?}", row.id);
            let spec = plan
                .iter()
                .find(|s| s.id == row.id)
                .unwrap_or_else(|| panic!("{what} is planned whole-tree under --full"));
            assert_eq!(row.build.argv()[1..], row.build_args[..], "{what}");
            assert!(row.build.demoted, "{what}: the build only compiles");
            assert!(!row.suite.demoted, "{what}: the suite runs code");
            assert_eq!(lane(&row.build), Some(drivers.clone()), "{what}");
            assert_eq!(lane(&row.build), lane_dir(&c, spec.lane), "{what}");
            assert!(
                row.bin.starts_with(&drivers) && !row.bin.starts_with(c.root.join("target")),
                "{what}: {}",
                row.bin.display()
            );
            match &row.handed {
                Handed::Discovered => {
                    assert_eq!(lane(&row.suite), lane(&row.build), "{what}: one dir");
                    assert_eq!(row.suite.envs, row.build.envs, "{what}: one environment");
                    // The ABSOLUTE pin: an equality alone passes an
                    // `ATERM_GUI_BIN` added to both commands, or to the lane
                    // itself — and the harness returns that variable before it
                    // refuses a stale binary.
                    assert_eq!(
                        row.suite.envs,
                        [
                            ("CARGO_TARGET_DIR".into(), drivers.clone().into_os_string()),
                            ("CARGO_BUILD_JOBS".into(), "8".into()),
                        ],
                        "{what}: the lane and nothing else — no ATERM_GUI_BIN bypass"
                    );
                }
                Handed::Env(var) => {
                    let set: Vec<(String, PathBuf)> = row
                        .suite
                        .envs
                        .iter()
                        .map(|(k, v)| (k.to_string_lossy().into_owned(), PathBuf::from(v)))
                        .collect();
                    assert_eq!(set, [((*var).to_string(), row.bin.clone())], "{what}");
                }
                Handed::Argv(args) => {
                    assert_eq!(row.suite.argv()[1..], args[..], "{what}");
                    assert!(row.suite.envs.is_empty(), "{what}: {:?}", row.suite.envs);
                }
            }
            if !matches!(row.handed, Handed::Discovered) {
                assert_eq!(
                    row.suite.term_grace,
                    Some(exec::TERM_GRACE),
                    "{what}: a killed suite is sent SIGTERM first, so its EXIT trap tears down"
                );
            }
            let text = std::fs::read_to_string(repo.join(row.source)).expect(row.source);
            for needle in row.reads {
                assert!(text.contains(needle), "{}: {needle:?}", row.source);
            }
        }
        // Negative control: the check tells spellings apart — the Codex lane
        // reads no `--binary` flag.
        let codex = std::fs::read_to_string(repo.join("tools").join(CODEX_LIVE_UPGRADE_SUITE))
            .expect("the Codex lane");
        assert!(!codex.contains("--binary)"));
    }

    /// The driver-builds row compiles exactly what the driver stages' own
    /// builds compile: the smoke binaries and the redraw harness everywhere,
    /// and on macOS, where they run, the live lanes' `aterm` and — in one
    /// invocation — every objc driver, the packages and examples of their
    /// single builds and nothing else.
    #[test]
    fn the_driver_prebuild_covers_exactly_the_driver_stages_build_argvs() {
        use std::collections::BTreeSet;
        let c = ctx(Scope::workspace());
        let cmds = driver_build_cmds(&c);
        let children: BTreeSet<Vec<String>> = cmds
            .iter()
            .map(|(_, cmd)| cmd.argv()[1..].to_vec())
            .collect();
        assert_eq!(children.len(), cmds.len(), "no child twice");
        // In the order OBJC_DRIVER_EXAMPLES lists them, which is stage order.
        let stage_builds = [
            objc_class_audit_build_args(),
            objc_ime_drive_build_args(),
            objc_toolbar_drive_build_args(),
            objc_window_drive_build_args(),
            objc_event_drive_build_args(),
            objc_bound_drive_build_args(),
        ];
        assert_eq!(stage_builds.len(), OBJC_DRIVER_EXAMPLES.len());
        for (single, (pkg, example)) in stage_builds.iter().zip(OBJC_DRIVER_EXAMPLES) {
            assert_eq!(
                single,
                &[
                    "--unverified",
                    "build",
                    "-q",
                    "-p",
                    pkg,
                    "--example",
                    example
                ]
            );
        }
        let values = |a: &[String], flag: &str| -> BTreeSet<String> {
            a.windows(2)
                .filter(|w| w[0] == flag)
                .map(|w| w[1].clone())
                .collect()
        };
        let combined = objc_driver_prebuild_args();
        let pkgs: BTreeSet<String> = stage_builds.iter().flat_map(|a| values(a, "-p")).collect();
        let examples: BTreeSet<String> = stage_builds
            .iter()
            .flat_map(|a| values(a, "--example"))
            .collect();
        assert_eq!(combined[..3], ["--unverified", "build", "-q"]);
        assert_eq!(values(&combined, "-p"), pkgs);
        assert_eq!(values(&combined, "--example"), examples);
        assert_eq!(
            combined.len(),
            3 + 2 * pkgs.len() + 2 * examples.len(),
            "nothing but the verb, the packages and the examples: {combined:?}"
        );
        let mut want: BTreeSet<Vec<String>> =
            [smoke_build_args(), redraw_conformance_build_args()].into();
        if cfg!(target_os = "macos") {
            want.insert(live_aterm_build_args());
            want.insert(combined);
        }
        assert_eq!(children, want);
    }

    /// A LIVE LANE'S NOT-RUN CODE IS NEVER A PASS. Before 2026-09-26 the two
    /// lanes were run by hand or not at all, and by hand each answered the
    /// missing `<root>/target/debug/aterm` with its not-run code — the handback
    /// lane `2`, the Codex lane `77` ("SKIP") — which a gate reading "nonzero is
    /// a failure, zero a pass" would get half right and one reading "only 1 is
    /// a failure" would pass. The handback lane's `2` is COULD NOT RUN; the
    /// Codex lane's `77`, in the `--full` tier it lives in, is a NAMED SKIP with
    /// the lane's own reason (counted, and forfeiting the run's contract claim,
    /// as the trust-mc floor's absent prover is). Each code only for the lane
    /// that declares it: the other lane's code is a finding.
    #[test]
    fn a_live_lanes_not_run_code_is_never_a_pass_and_quotes_the_lanes_reason() {
        use crate::Outcome;
        let fh = FOREGROUND_HANDBACK_SUITE;
        let cx = CODEX_LIVE_UPGRADE_SUITE;
        assert_eq!(
            live_aterm_outcome(fh, Some(0), "39 ok, 0 FAIL, 1 skip"),
            (Outcome::Ok, fh.to_string())
        );
        assert_eq!(
            live_aterm_outcome(cx, Some(0), "PASS"),
            (Outcome::Ok, cx.to_string())
        );
        for name in LIVE_ATERM_SUITES {
            assert_eq!(
                live_aterm_outcome(name, Some(1), "").0,
                Outcome::Fail(Severity::GateFailed),
                "{name}"
            );
            assert_eq!(
                live_aterm_outcome(name, None, "").0,
                Outcome::Fail(Severity::CouldNotRun),
                "{name}: a signal decided nothing"
            );
        }
        // The Codex lane's reason is the label, so the verdict names the remedy.
        let transcript = "managed Codex 0.157.1\nSKIP: the store holds no Codex older than 0.157.1 to upgrade from\n";
        assert_eq!(
            live_aterm_outcome(cx, Some(77), transcript),
            (
                Outcome::Skip,
                "test-codex-live-upgrade.sh: NOT RUN — the store holds no Codex older than 0.157.1 \
                 to upgrade from (exit 77: this machine lacks a prerequisite; a named skip, never a \
                 pass)"
                    .to_string()
            )
        );
        let (outcome, label) = live_aterm_outcome(cx, Some(77), "");
        assert_eq!(outcome, Outcome::Skip);
        assert!(
            label.contains("the lane skipped without saying why"),
            "{label}"
        );
        let (outcome, label) = live_aterm_outcome(
            fh,
            Some(2),
            "NOT RUN: no aterm binary at /x (targo --unverified build -p aterm)\n",
        );
        assert_eq!(outcome, Outcome::Fail(Severity::CouldNotRun));
        assert_eq!(
            label,
            "test-foreground-handback.sh: NOT RUN — no aterm binary at /x (targo --unverified \
             build -p aterm) (exit 2, never a pass)"
        );
        // A boot failure is the lane's FAIL row and exit 1 — a finding.
        assert_eq!(
            live_aterm_outcome(
                fh,
                Some(1),
                "FAIL  boot: /bin/zsh — the headless instance exited before it answered\n"
            )
            .0,
            Outcome::Fail(Severity::GateFailed)
        );
        // Each lane's not-run code is its own; the other's is a finding.
        assert_eq!(
            live_aterm_outcome(fh, Some(77), "").0,
            Outcome::Fail(Severity::GateFailed)
        );
        assert_eq!(
            live_aterm_outcome(cx, Some(2), "").0,
            Outcome::Fail(Severity::GateFailed)
        );
    }
}
