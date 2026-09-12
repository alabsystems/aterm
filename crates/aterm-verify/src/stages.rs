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
//! nothing exists), `ATERM_SEARCH_REGEX_LANE=1` on the regex lane, and `--unverified` on
//! every driver invocation (naming the lane is the point: `targo` REFUSES a bare
//! verb precisely so a gate cannot be quietly unverified).

use crate::exec::{self, Capture, Cmd};
use crate::ladder::{Outcome, Report, Severity};
use crate::plan::{StageId, StageSpec};
use crate::scope::Scope;
use crate::smoke::{debug_bin, debug_example};
use crate::smoke_stages;
use crate::{Ctx, have_on_path, is_executable_file};

/// Dispatch one stage.
#[must_use]
pub fn run_stage(ctx: &Ctx, spec: &StageSpec) -> Report {
    let mut r = Report::new(spec.title.clone());
    match spec.id {
        StageId::Build => build(ctx, &mut r),
        StageId::Test => test(ctx, &mut r),
        StageId::Doctests => doctests(ctx, &mut r),
        StageId::RegexLane => regex_lane(ctx, &mut r),
        StageId::Tippy => tippy(ctx, &mut r),
        StageId::Formatting => formatting(ctx, &mut r),
        StageId::GrepGuards => grep_guards(ctx, &mut r),
        StageId::InstallChannel => install_channel(ctx, &mut r),
        StageId::AtpkgTooling => atpkg_tooling(ctx, &mut r),
        StageId::TrustGateVerdict => trust_gate_verdict(ctx, &mut r),
        StageId::TrustContractProbe => trust_contract_probe(ctx, &mut r),
        StageId::StartCompare => start_compare(ctx, &mut r),
        StageId::LicenseHeaders => license_headers(ctx, &mut r),
        StageId::FeatureGates => feature_gates(ctx, &mut r),
        StageId::LibcOracle => libc_oracle(ctx, &mut r),
        StageId::FreezeGate => freeze_gate(ctx, &mut r),
        StageId::ProofInventory => proof_inventory(ctx, &mut r),
        StageId::ControlSocketSmoke => smoke_stages::control_socket_smoke(ctx, &mut r),
        StageId::GuiSmoke => smoke_stages::gui_typing_smoke(ctx, &mut r),
        StageId::RedrawConformance => redraw_conformance(ctx, &mut r),
        StageId::ObjcClassAudit => objc_class_audit(ctx, &mut r),
        StageId::ObjcImeDrive => objc_ime_drive(ctx, &mut r),
        StageId::ObjcToolbarDrive => objc_toolbar_drive(ctx, &mut r),
        StageId::ObjcWindowDrive => objc_window_drive(ctx, &mut r),
        StageId::ObjcEventDrive => objc_event_drive(ctx, &mut r),
        StageId::ObjcAlertDrive => objc_alert_drive(ctx, &mut r),
        StageId::ObjcSwizzleDrive => objc_swizzle_drive(ctx, &mut r),
        StageId::ObjcBoundDrive => objc_bound_drive(ctx, &mut r),
        StageId::DifferentialOracle => differential_oracle(ctx, &mut r),
        StageId::KaniFloor => kani_floor(ctx, &mut r),
        StageId::CrossCells => cross_cells(ctx, &mut r),
    }
    r
}

// ---------------------------------------------------------------------------
// The argv builders. Pure, so the port is a test and not a promise.
// ---------------------------------------------------------------------------

/// `targo --unverified build <scope>`
#[must_use]
pub fn build_args(scope: &Scope) -> Vec<String> {
    let mut a = vec!["--unverified".to_string(), "build".to_string()];
    a.extend(scope.args());
    a
}

/// `targo --unverified test <scope> --no-fail-fast`
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
#[must_use]
pub fn test_args(scope: &Scope) -> Vec<String> {
    let mut a = vec!["--unverified".to_string(), "test".to_string()];
    a.extend(scope.args());
    a.push("--no-fail-fast".to_string());
    a
}

/// `targo --unverified test --doc <scope> --no-fail-fast` — run explicitly,
/// because the unit
/// stage's `targo test` can skip documentation examples when scoped or under a
/// nextest-style runner, and doctests then rot silently. `--no-fail-fast` for
/// the reason [`test_args`] gives: one crate's failing doctest must not hide
/// every crate cargo had not reached yet.
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

/// `targo --unverified test -p aterm-search --features regex --no-fail-fast`
#[must_use]
pub fn regex_lane_args() -> Vec<String> {
    [
        "--unverified",
        "test",
        "-p",
        "aterm-search",
        "--features",
        "regex",
        "--no-fail-fast",
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
/// against. `gate lint` says so out loud rather than implying otherwise.
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
/// `xtask`'s `the_gated_feature_table_matches_every_required_features_target`,
/// so a seventh gated target cannot be added without either extending this or
/// reddening that test.
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
/// is one re-lint of the two named packages against a wider feature set; every
/// dependency below them is a cache hit from the first pass.
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
pub const KANI_CRATES: [&str; 3] = ["aterm-parser", "aterm-render", "aterm-uds"];

/// The exact command one Kani floor run spawns. `KANI_CRATE` selects which
/// crate's proofs the script drives; lose it and every iteration of the loop
/// runs the same default, so two of the three crates go unproven while the
/// ladder still prints three green rows. `TRUST_MC_SYSROOT` / `AY_BIN_DIR`
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
///
/// 1. `$TRUST_MC_SYSROOT` — an explicit location, never fallen back from.
/// 2. `<atpkg prefix>/store/trust-mc/current` — what `aterm pkg install
///    trust-mc` lays down, and the only sysroot most machines have. Taken when
///    it carries the driver (`bin/trust-mc-driver`); the managed bundle ships
///    no `cargo-trust-mc` name — the script derives that symlink OUTSIDE the
///    store, which is tree_root-attested and immutable.
/// 3. `$HOME/trust/first-party/trust-mc/target/trust-mc` — a from-source dev build.
#[must_use]
pub fn trust_mc_sysroot(env: &crate::EnvSnapshot) -> std::path::PathBuf {
    if let Some(explicit) = &env.trust_mc_sysroot {
        return explicit.clone();
    }
    let store = crate::toolchain::atpkg_prefix(&env.home, env.xdg_config_home.as_deref())
        .join("store/trust-mc/current");
    if is_executable_file(&store.join("bin/trust-mc-driver"))
        || is_executable_file(&store.join("bin/cargo-trust-mc"))
    {
        return store;
    }
    env.home.join("trust/first-party/trust-mc/target/trust-mc")
}

/// Where the `ay` solver lives, in the script's order: `$AY_BIN_DIR`, else the
/// atpkg shim dir (`<prefix>/bin`, where `aterm pkg install ay` shims it), else
/// the from-source dev build `$HOME/trust/first-party/ay/target/release`.
#[must_use]
pub fn ay_bin_dir(env: &crate::EnvSnapshot) -> std::path::PathBuf {
    if let Some(explicit) = &env.ay_bin_dir {
        return explicit.clone();
    }
    let shims =
        crate::toolchain::atpkg_prefix(&env.home, env.xdg_config_home.as_deref()).join("bin");
    if is_executable_file(&shims.join("ay")) {
        return shims;
    }
    env.home.join("trust/first-party/ay/target/release")
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
/// some rows had no authority on this host because a protocol the class claims
/// is one its AppKit does not register (aterm supplies a name-only stand-in —
/// macOS 14.4.1's `NSApplicationDelegate`); read as green it would restore
/// exactly the silence this gate exists to remove — the two plants it was
/// built against both left a GREEN build behind them.
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
            "objc live-class audit: NOT RUN — no event loop, no delegate was installed, or rows whose claimed protocol this host's AppKit does not register (aterm's name-only stand-in declares nothing; the auditor's NOT CHECKED lines name them), so the registered class was not fully proven (exit 2, never a pass)".to_string(),
        ),
        Some(c) => (
            Outcome::Fail(Severity::GateFailed),
            format!("objc live-class audit: unexpected exit {c} (the auditor answers only 0/1/2)"),
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

/// `targo --unverified test -p aterm-bench --test differential`
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

/// The modal driver's target name — the `[[example]]`, the built file and the
/// argv below all have to agree, so they read it from here.
pub const OBJC_ALERT_DRIVE_EXAMPLE: &str = "objc_alert_drive";

/// `targo --unverified build -q -p aterm-gui --example objc_alert_drive`
#[must_use]
pub fn objc_alert_drive_build_args() -> Vec<String> {
    [
        "--unverified",
        "build",
        "-q",
        "-p",
        "aterm-gui",
        "--example",
        OBJC_ALERT_DRIVE_EXAMPLE,
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// How the ladder reads the modal driver's exit code (`0` clean / `1` finding
/// / `2` NOT RUN, declared in `crates/aterm-gui/examples/objc_alert_drive.rs`).
///
/// THREE codes, like the window drive's: the driver's 90 s budget ends in `2`,
/// not in a code of its own, because a sheet that never came down proved
/// nothing about the subsystem rather than something bad about it.
#[must_use]
pub fn objc_alert_outcome(code: Option<i32>) -> (Outcome, String) {
    match code {
        Some(0) => (
            Outcome::Ok,
            "objc alert drive: the real NSAlert answered every question — +new and the button order, the first button's bare-Return key equivalent read off AppKit, the block ABI called directly, the sheet attaching, a keyDown through the swizzled -sendEvent: reaching the installed monitor and consumed by its nil, removeMonitor:, performClick: ending the sheet with NSAlertFirstButtonReturn delivered to a block AppKit had copied, the menu bar walk, and a chrome capture that is a PNG".to_string(),
        ),
        Some(1) => (
            Outcome::Fail(Severity::GateFailed),
            "objc alert drive: the driven alert did not behave — a button, key equivalent, monitor, completion response, sheet predicate, menu-bar read or capture answered the wrong value".to_string(),
        ),
        Some(2) => (
            Outcome::Fail(Severity::CouldNotRun),
            "objc alert drive: NOT RUN — no event loop, no window, or the stages did not finish within the driver's budget, so nothing was proven about the modal subsystem (exit 2, never a pass)".to_string(),
        ),
        Some(c) => (
            Outcome::Fail(Severity::GateFailed),
            format!("objc alert drive: unexpected exit {c} (the driver answers only 0/1/2) — a signal here is the shape of a block or monitor ownership bug"),
        ),
        None => (
            Outcome::Fail(Severity::CouldNotRun),
            "objc alert drive: no exit status — killed by a signal, or never spawned".to_string(),
        ),
    }
}

/// The swizzle driver's target name — the built file and the argv below have
/// to agree, so they read it from here. An `aterm-objc` example: the
/// capability under drive is the crate's own, and the example carries its
/// AppKit link line itself.
pub const OBJC_SWIZZLE_DRIVE_EXAMPLE: &str = "objc_swizzle_drive";

/// `targo --unverified build -q -p aterm-objc --example objc_swizzle_drive`
#[must_use]
pub fn objc_swizzle_drive_build_args() -> Vec<String> {
    [
        "--unverified",
        "build",
        "-q",
        "-p",
        "aterm-objc",
        "--example",
        OBJC_SWIZZLE_DRIVE_EXAMPLE,
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// How the ladder reads the swizzle driver's exit code (`0` clean / `1`
/// finding / `2` NOT RUN, declared in
/// `crates/aterm-objc/examples/objc_swizzle_drive.rs`).
#[must_use]
pub fn objc_swizzle_outcome(code: Option<i32>) -> (Outcome, String) {
    match code {
        Some(0) => (
            Outcome::Ok,
            "objc swizzle drive: SwizzleSite installed on the live -[NSApplication sendEvent:] — the prototype check passed against Apple's own v24@0:8@16, the registered encoding was unchanged by the swap (the measurement that makes encoding checks no evidence of a swizzle), the IMP's image moved from AppKit into this executable, a real NSEvent ran both halves of the chain, and a wrong prototype was refused".to_string(),
        ),
        Some(1) => (
            Outcome::Fail(Severity::GateFailed),
            "objc swizzle drive: the swizzle did not behave — the prototype check disagreed with Apple's encoding, the IMP's image did not move, the chain did not run both halves, or a wrong prototype was accepted".to_string(),
        ),
        Some(2) => (
            Outcome::Fail(Severity::CouldNotRun),
            "objc swizzle drive: NOT RUN — no NSApplication or no main thread, so nothing was proven about the swizzle (exit 2, never a pass)".to_string(),
        ),
        Some(c) => (
            Outcome::Fail(Severity::GateFailed),
            format!("objc swizzle drive: unexpected exit {c} (the driver answers only 0/1/2) — a signal here is the shape of a chain entered with the wrong prototype"),
        ),
        None => (
            Outcome::Fail(Severity::CouldNotRun),
            "objc swizzle drive: no exit status — killed by a signal, or never spawned".to_string(),
        ),
    }
}

/// The container driver's target name — the built file and the argv below
/// have to agree, so they read it from here. An `aterm-objc` example, like the
/// swizzle driver's.
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

#[must_use]
pub fn differential_args() -> Vec<String> {
    [
        "--unverified",
        "test",
        "-p",
        "aterm-bench",
        "--test",
        "differential",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// The nested workspace's checked-in driver is the oracle contract. Keeping
/// this as one argv value (with no cargo fallback or reimplementation here)
/// means changes to its target-cell matrix automatically reach the required
/// merge gate. The target dir is absolute because `run.sh` deliberately runs
/// cross-cell Cargo commands from `/`; inheriting a relative caller value would
/// turn that into `/target`, while inheriting an arbitrary absolute one would
/// collapse this scheduler lane onto somebody else's Cargo lock.
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
/// and 3 means a preflight/environment inability decided nothing. A missing
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

/// The script's `run()`: under `--selftest` say so and execute nothing;
/// otherwise run, print what the child said, and decide.
fn run_labeled(ctx: &Ctx, r: &mut Report, label: &str, cmd: &Cmd) {
    if ctx.selftest {
        r.skip(format!("{label} (selftest: not executed)"));
        return;
    }
    let out = exec::run(cmd, ctx.exec_env());
    r.raw(out.output.as_str());
    r.decide(out.ok, label);
}

/// The script's `run_scoped()`: `run_labeled` for the stages that COMPILE the
/// selected crates.
///
/// An empty selection must not fall through to a bare `targo build` — with no
/// `-p` that builds the whole default workspace, which would be a full build
/// wearing a narrow run's label. Only `--changed` can select nothing (a
/// docs-only branch), and it is an honest skip, counted and named like any
/// other, so the verdict says the run compiled nothing.
fn run_scoped(ctx: &Ctx, r: &mut Report, label: &str, cmd: &Cmd) {
    if ctx.scope.selects_nothing() {
        r.skip(format!("{label} (change-scoped run selected no crates)"));
        return;
    }
    run_labeled(ctx, r, label, cmd);
}

/// A `targo` invocation, always naming its lane.
fn targo(ctx: &Ctx, args: Vec<String>) -> Cmd {
    Cmd::new(&ctx.tools.targo).args(args)
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
/// run compiles no lib target (rustdoc is never spawned) or `--selftest`
/// executes nothing anyway; the stage arms below hold those qualifiers.
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
// 1) BUILD
// ---------------------------------------------------------------------------
fn build(ctx: &Ctx, r: &mut Report) {
    if ctx.tools.have_targo() {
        run_scoped(
            ctx,
            r,
            &format!("targo build {}", ctx.scope.label()),
            &targo(ctx, build_args(&ctx.scope)),
        );
    } else {
        // Fail-closed, and COULD-NOT-RUN rather than FAILED: nothing about the
        // tree was decided. Never a stock-cargo fallback — that would make the
        // gate quietly unverified, which is what the two-lane driver prevents.
        r.cannot_run(ctx.tools.missing_targo_label());
    }
}

// ---------------------------------------------------------------------------
// 2) TEST — `targo test` runs crate doctests after the unit/integration targets,
//    so trustdoc is bound here as well as in the explicit doc-only stage — and a
//    machine with NO doc driver anywhere is diagnosed here (COULD-NOT-RUN with
//    the remedy), not left to die at exec mid-stage after the unit tests passed.
// ---------------------------------------------------------------------------
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
    let cmd = targo(ctx, test_args(&ctx.scope));
    match doc_driver(ctx) {
        DocDriver::Stage2 => run_labeled(
            ctx,
            r,
            &format!("{label} (trustdoc)"),
            &with_trustdoc(ctx, cmd),
        ),
        DocDriver::Ambient => {
            run_labeled(ctx, r, &format!("{label} (caller's RUSTDOC)"), &cmd);
        }
        DocDriver::BarePath => run_labeled(ctx, r, &label, &cmd),
        // Diagnose only a run that would really spawn rustdoc: `--selftest`
        // executes nothing (run_labeled prints its skip), and a scope that
        // compiles no lib target compiles no doctests, so the child runs
        // green without a doc driver — declaring the machine broken there
        // would blame a tool the run never needed.
        DocDriver::Absent if ctx.selftest || !scope_compiles_doctests(ctx) => {
            run_labeled(ctx, r, &label, &cmd);
        }
        DocDriver::Absent => r.cannot_run(ctx.tools.missing_trustdoc_label()),
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
        DocDriver::Stage2 => run_labeled(
            ctx,
            r,
            &format!("{label} (trustdoc)"),
            &with_trustdoc(ctx, cmd),
        ),
        DocDriver::Ambient => {
            run_labeled(ctx, r, &format!("{label} (caller's RUSTDOC)"), &cmd);
        }
        DocDriver::BarePath => run_labeled(ctx, r, &label, &cmd),
        // Selftest executes nothing — run_labeled prints its uniform skip.
        DocDriver::Absent if ctx.selftest => run_labeled(ctx, r, &label, &cmd),
        // The test stage directly above already declared the COULD-NOT-RUN with
        // the full remedy — same pattern as the no-targo ladder, where build
        // declares once and the later stages skip pointing at it. (Whenever
        // this stage survives its lib-target guard, that scope made the test
        // stage's Absent arm a real cannot_run, so the pointer never dangles.)
        DocDriver::Absent => r.skip("targo test --doc (no doc driver — see the test line)"),
    }
}

// ---------------------------------------------------------------------------
// 2.6) REGEX SEARCH LANE. `aterm-search`'s regex-mode oracle battery is gated on
//    `feature = "regex"`, which the default test stage does NOT enable — without
//    this stage the whole battery compiles out to ZERO cases and the suite stays
//    green with no regex coverage. The `ATERM_SEARCH_REGEX_LANE` marker arms the
//    always-compiled `regex_lane_tripwire`, which hard-fails if the marker is set
//    but the feature was dropped, so the lane cannot silently lose its coverage.
// ---------------------------------------------------------------------------
/// The exact command the regex lane spawns — extracted so a test asserts on it
/// rather than on a replica. The marker is the whole point of the stage: without
/// `ATERM_SEARCH_REGEX_LANE` the suite still passes, green with no regex
/// coverage at all.
fn regex_lane_cmd(ctx: &Ctx) -> Cmd {
    targo(ctx, regex_lane_args()).env("ATERM_SEARCH_REGEX_LANE", "1")
}

fn regex_lane(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        r.skip("regex search lane (no targo)");
        return;
    }
    let label = "targo test -p aterm-search --features regex";
    let cmd = regex_lane_cmd(ctx);
    // This lane compiles aterm-search's doctests too, so it takes the same
    // doc-driver rule as the test/doctest stages — left on the old two-way
    // binding it would be the one stage still dying raw at rustdoc exec on a
    // machine with no doc driver, and worse, dying as a GateFailed that
    // outranks the test stage's honest COULD-NOT-RUN in the verdict.
    match doc_driver(ctx) {
        DocDriver::Stage2 => run_labeled(
            ctx,
            r,
            &format!("{label} (trustdoc)"),
            &with_trustdoc(ctx, cmd),
        ),
        DocDriver::Ambient => {
            run_labeled(ctx, r, &format!("{label} (caller's RUSTDOC)"), &cmd);
        }
        DocDriver::BarePath => run_labeled(ctx, r, label, &cmd),
        DocDriver::Absent if ctx.selftest => run_labeled(ctx, r, label, &cmd),
        // Planned only when aterm-search (a lib crate) is in scope, so the test
        // stage's Absent arm was a real cannot_run — the pointer never dangles.
        DocDriver::Absent => r.skip(format!("{label} (no doc driver — see the test line)")),
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
}

fn tippy(ctx: &Ctx, r: &mut Report) {
    if ctx.selftest {
        r.skip("tippy lint (selftest: not executed)");
        return;
    }
    let Some(bin) = ctx.tools.tippy.clone() else {
        r.skip(ctx.tools.missing_tippy_label());
        return;
    };
    run_scoped(
        ctx,
        r,
        &format!("tippy {} -D warnings", ctx.scope.label()),
        &tippy_cmd(ctx, &bin, tippy_args(&ctx.scope)),
    );
    // THE SECOND PASS IS NOT OPTIONAL POLISH. `--all-targets` above built no
    // target whose `required-features` are off, so without this the six in
    // [`GATED_LINT_FEATURES`] are linted by nobody — which is how a broken
    // bench build survived four days. Its own row, so the ladder shows whether
    // it ran.
    if let Some(args) = tippy_gated_args(&ctx.scope) {
        run_scoped(
            ctx,
            r,
            "tippy required-features targets -D warnings",
            &tippy_cmd(ctx, &bin, args),
        );
    }
}

// ---------------------------------------------------------------------------
// 3) GREP GUARDS (zero-tolerance, always whole-tree)
// ---------------------------------------------------------------------------
/// FORMATTING — `xtask gate lint --fmt-only`, i.e. the formatter lane's BOTH
/// passes and no other lane.
///
/// This stage did not exist until 2026-08-31, and the gap was declared rather
/// than hidden: `verify.sh` ran a tippy stage and no fmt stage, and said so.
/// Declaring a limit is not covering it. `.githooks/pre-push` has been advisory
/// since 2026-08-24, so between those two facts NOTHING in this repository ran
/// the formatter unless a human chose to — and the MEASURED consequence was
/// three consecutive rebases of `main` arriving with drift (5 files, 2, 1), one
/// of them in `aterm-link`, a crate outside `members = ["crates/*"]` that
/// `targo-fmt --all` structurally cannot see.
///
/// It is cheap enough to be uncontroversial: the check needs no compiler —
/// trustfmt parses and prints, it does not build — and cost 7.5 s over 1,761
/// tracked files on two measured runs. The `MainTarget` lane is for the xtask
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
    run_labeled(
        ctx,
        r,
        "gate lint --fmt-only",
        &targo(ctx, xtask_gate_args_with("lint", &["--fmt-only"])),
    );
}

fn grep_guards(ctx: &Ctx, r: &mut Report) {
    let g = ctx.tools_dir().join("grep_guard.sh");
    if !is_executable_file(&g) {
        r.cannot_run(format!(
            "grep_guard.sh missing or not executable ({})",
            g.display()
        ));
        return;
    }
    if ctx.selftest {
        r.skip("grep_guard.sh (selftest)");
        return;
    }
    let out = exec::run(&script_cmd(&g, &ctx.root), ctx.exec_env());
    r.raw(out.output.as_str());
    r.decide(out.ok, "grep_guard.sh");
}

// ---------------------------------------------------------------------------
// 3.5) BOOTSTRAP UPDATE CHANNEL — keep tools/install.sh aligned with the in-app
//    updater's complete-catalog numeric arbitration and exact asset identity.
// ---------------------------------------------------------------------------
fn install_channel(ctx: &Ctx, r: &mut Report) {
    let t = ctx.tools_dir().join("test-install-channel.sh");
    if is_executable_file(&t) {
        run_labeled(ctx, r, "test-install-channel.sh", &Cmd::new(&t));
    } else {
        r.cannot_run(format!(
            "test-install-channel.sh missing or not executable ({})",
            t.display()
        ));
    }
}

// ---------------------------------------------------------------------------
// 3.52) ATPKG PUBLISH TOOLING — the two deterministic shell suites over the
//    producer scripts (tools/atpkg-author-vendor.sh, atpkg-index.sh,
//    atpkg-publish.sh, atpkg-mirror-public.sh and the vendor lane
//    tools/atpkg-auto-vendor.sh). Every vendor, key and gh call is stubbed
//    (their headers say so): no network, no token, no repo mutation. Until
//    2026-09-08 neither suite ran under any gate, so a change to the scripts
//    that sign the toolchain index could land unmeasured (the audit finding).
//    Same posture as install_channel: a missing suite is a cannot-run, never a
//    skip.
// ---------------------------------------------------------------------------
fn atpkg_tooling(ctx: &Ctx, r: &mut Report) {
    for name in ["test-atpkg-vendor-tooling.sh", "test-atpkg-auto-vendor.sh"] {
        let t = ctx.tools_dir().join(name);
        if is_executable_file(&t) {
            run_labeled(ctx, r, name, &Cmd::new(&t));
        } else {
            r.cannot_run(format!(
                "{name} missing or not executable ({})",
                t.display()
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// 3.55) TRUST-GATE VERDICT SELF-TEST — tools/trust-gate-all.sh prints the
//    sentence that IS the campaign claim ("100% MACHINE-PROVED (workspace +
//    every vendored fork, …)") and has several inputs that shrink the run.
//    Until its self-test existed the verdict logic had never been exercised
//    against a narrowed run at all, and it printed the workspace sentence for
//    runs that were not the workspace. It now also covers the gate LIST: that
//    members are addressed `-p name@version` and forks by manifest path, and
//    that a fork can neither appear in the resolved graph undeclared nor vanish
//    from it while the roster still lists it. Hard-required, exactly like
//    test-install-channel.sh: a missing self-test is not a skip, because the
//    thing it guards is a claim.
// ---------------------------------------------------------------------------
fn trust_gate_verdict(ctx: &Ctx, r: &mut Report) {
    let t = ctx.tools_dir().join("test-trust-gate-verdict.sh");
    if is_executable_file(&t) {
        run_labeled(ctx, r, "test-trust-gate-verdict.sh", &Cmd::new(&t));
    } else {
        r.cannot_run(format!(
            "test-trust-gate-verdict.sh missing or not executable ({})",
            t.display()
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
// 3.6) STARTUP COMPARISON SCHEDULER — the publishable-startup evidence path must
//    fail closed on malformed samples, mutable harness bytes, uncertain thermal
//    state, identical-artifact controls, and timed-out process descendants.
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
// 4) LICENSE / SPDX HEADERS (every .rs carries the two-line header)
// ---------------------------------------------------------------------------
fn license_headers(ctx: &Ctx, r: &mut Report) {
    let lic = ctx.tools_dir().join("license_check.sh");
    if !is_executable_file(&lic) {
        r.cannot_run(format!(
            "license_check.sh missing or not executable ({})",
            lic.display()
        ));
        return;
    }
    if ctx.selftest {
        r.skip("license_check.sh (selftest)");
        return;
    }
    let out = exec::run(&script_cmd(&lic, &ctx.root), ctx.exec_env());
    r.raw(out.output.as_str());
    r.decide(out.ok, "license_check.sh");
}

// ---------------------------------------------------------------------------
// 4.5) FEATURE GATES (advertise-vs-implement + dormant-feature detection), plus
//    the main-loop census: the ONLY enforced net for the multi-line bound-guard
//    form `let g = term_lock(..); g.resize(..)` that the single-line grep
//    tripwire cannot see.
// ---------------------------------------------------------------------------
fn feature_gates(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        r.skip("feature gates (no targo)");
        return;
    }
    for gate in ["drift", "dormant", "mainloop"] {
        run_labeled(
            ctx,
            r,
            &format!("gate {gate}"),
            &targo(ctx, xtask_gate_args(gate)),
        );
    }
}

// ---------------------------------------------------------------------------
// 4.5a) FIRST-PARTY LIBC ABI ORACLE. The const/layout/type assertions compile
//    for every target cell, the emitted-symbol gate closes link-name aliases,
//    and `cargo test` executes the pointer-valued and C-macro checks for the
//    host's native cell. Therefore a native Linux run is the required Linux
//    runtime route; cross-compiling that cell alone is deliberately not enough.
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
    let label = "libc-oracle/run.sh (cross-cell ABI + native runtime)";
    if ctx.selftest {
        r.skip(format!("{label} (selftest: not executed)"));
        return;
    }
    let out = exec::run(&cmd, ctx.exec_env());
    r.raw(out.output.as_str());
    r.record(libc_oracle_outcome(out.code), label);
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
        &targo(ctx, freeze_gate_args()),
    );
}

// ---------------------------------------------------------------------------
// 4.6) COMPUTED-ONLY PROOF INVENTORY — count the proof attributes, fail on scan
//    errors or an empty inventory, and reject a hand-maintained README total.
// ---------------------------------------------------------------------------
fn proof_inventory(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        r.skip("gate counts (no targo)");
        return;
    }
    run_labeled(
        ctx,
        r,
        "gate counts",
        &targo(ctx, xtask_gate_args("counts")),
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
    if ctx.selftest {
        r.skip("redraw conformance (selftest: not executed)");
        return;
    }
    if !ctx.tools.have_targo() {
        // The build stage already reported COULD-NOT-RUN for the same absence;
        // naming it again here keeps the skip counted and the verdict narrowed.
        r.skip("redraw conformance (no targo)");
        return;
    }
    let build = exec::run(&targo(ctx, redraw_conformance_build_args()), ctx.exec_env());
    if !build.ok {
        r.raw(build.output.as_str());
        r.fail(format!("targo build --bin {REDRAW_CONFORMANCE_BIN}"));
        return;
    }
    let bin = debug_bin(
        &ctx.root,
        ctx.env.cargo_target_dir.as_deref(),
        REDRAW_CONFORMANCE_BIN,
    );
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
    r.record(outcome, label);
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
    // SELFTEST FIRST, so the `--selftest` ladder reads the same on every
    // platform — a mode whose whole contract is "execute nothing" must not
    // report a different reason per host.
    if ctx.selftest {
        r.skip("objc live-class audit (selftest: not executed)");
        return;
    }
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
    let build = exec::run(&targo(ctx, objc_class_audit_build_args()), ctx.exec_env());
    if !build.ok {
        r.raw(build.output.as_str());
        r.fail(format!("targo build --example {OBJC_CLASS_AUDIT_EXAMPLE}"));
        return;
    }
    let bin = debug_example(
        &ctx.root,
        ctx.env.cargo_target_dir.as_deref(),
        OBJC_CLASS_AUDIT_EXAMPLE,
    );
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
    r.record(outcome, label);
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
    // SELFTEST FIRST, for the same reason the auditor does it.
    if ctx.selftest {
        r.skip("objc IME drive (selftest: not executed)");
        return;
    }
    if !cfg!(target_os = "macos") {
        r.skip("objc IME drive (macOS only: the driven class is a macOS one)");
        return;
    }
    if !ctx.tools.have_targo() {
        r.skip("objc IME drive (no targo)");
        return;
    }
    let build = exec::run(&targo(ctx, objc_ime_drive_build_args()), ctx.exec_env());
    if !build.ok {
        r.raw(build.output.as_str());
        r.fail(format!("targo build --example {OBJC_IME_DRIVE_EXAMPLE}"));
        return;
    }
    let bin = debug_example(
        &ctx.root,
        ctx.env.cargo_target_dir.as_deref(),
        OBJC_IME_DRIVE_EXAMPLE,
    );
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
    r.record(outcome, label);
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
    // SELFTEST FIRST, for the same reason the auditor does it: a mode whose
    // whole contract is "execute nothing" must not report a different reason
    // per host.
    if ctx.selftest {
        r.skip("objc toolbar drive (selftest: not executed)");
        return;
    }
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
    let build = exec::run(&targo(ctx, objc_toolbar_drive_build_args()), ctx.exec_env());
    if !build.ok {
        r.raw(build.output.as_str());
        r.fail(format!(
            "targo build --example {OBJC_TOOLBAR_DRIVE_EXAMPLE}"
        ));
        return;
    }
    let bin = debug_example(
        &ctx.root,
        ctx.env.cargo_target_dir.as_deref(),
        OBJC_TOOLBAR_DRIVE_EXAMPLE,
    );
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
    r.record(outcome, label);
}

fn objc_window_drive(ctx: &Ctx, r: &mut Report) {
    // SELFTEST FIRST, as its three siblings do.
    if ctx.selftest {
        r.skip("objc window drive (selftest: not executed)");
        return;
    }
    if !cfg!(target_os = "macos") {
        r.skip("objc window drive (macOS only: the driven window_delegate.rs is a macOS one)");
        return;
    }
    if !ctx.tools.have_targo() {
        r.skip("objc window drive (no targo)");
        return;
    }
    let build = exec::run(&targo(ctx, objc_window_drive_build_args()), ctx.exec_env());
    if !build.ok {
        r.raw(build.output.as_str());
        r.fail(format!("targo build --example {OBJC_WINDOW_DRIVE_EXAMPLE}"));
        return;
    }
    let bin = debug_example(
        &ctx.root,
        ctx.env.cargo_target_dir.as_deref(),
        OBJC_WINDOW_DRIVE_EXAMPLE,
    );
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
    r.record(outcome, label);
}

fn objc_event_drive(ctx: &Ctx, r: &mut Report) {
    // SELFTEST FIRST, as its three siblings do.
    if ctx.selftest {
        r.skip("objc event drive (selftest: not executed)");
        return;
    }
    if !cfg!(target_os = "macos") {
        r.skip("objc event drive (macOS only: the driven view.rs is a macOS one)");
        return;
    }
    if !ctx.tools.have_targo() {
        r.skip("objc event drive (no targo)");
        return;
    }
    let build = exec::run(&targo(ctx, objc_event_drive_build_args()), ctx.exec_env());
    if !build.ok {
        r.raw(build.output.as_str());
        r.fail(format!("targo build --example {OBJC_EVENT_DRIVE_EXAMPLE}"));
        return;
    }
    let bin = debug_example(
        &ctx.root,
        ctx.env.cargo_target_dir.as_deref(),
        OBJC_EVENT_DRIVE_EXAMPLE,
    );
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
    r.record(outcome, label);
}

fn objc_alert_drive(ctx: &Ctx, r: &mut Report) {
    // SELFTEST FIRST, as its siblings do.
    if ctx.selftest {
        r.skip("objc alert drive (selftest: not executed)");
        return;
    }
    if !cfg!(target_os = "macos") {
        r.skip("objc alert drive (macOS only: the driven modal subsystem is a macOS one)");
        return;
    }
    if !ctx.tools.have_targo() {
        r.skip("objc alert drive (no targo)");
        return;
    }
    let build = exec::run(&targo(ctx, objc_alert_drive_build_args()), ctx.exec_env());
    if !build.ok {
        r.raw(build.output.as_str());
        r.fail(format!("targo build --example {OBJC_ALERT_DRIVE_EXAMPLE}"));
        return;
    }
    let bin = debug_example(
        &ctx.root,
        ctx.env.cargo_target_dir.as_deref(),
        OBJC_ALERT_DRIVE_EXAMPLE,
    );
    if !is_executable_file(&bin) {
        r.cannot_run(format!(
            "objc alert drive: just-built driver missing ({})",
            bin.display()
        ));
        return;
    }
    // A BINARY, never `targo run`, for the same two reasons as its siblings:
    // the driver lane's banner would land in the transcript, and cargo's own
    // exit codes would collide with the driver's 0/1/2.
    let out = exec::run(&Cmd::new(&bin), ctx.exec_env());
    r.raw(out.output.as_str());
    let (outcome, label) = objc_alert_outcome(out.code);
    r.record(outcome, label);
}

fn objc_swizzle_drive(ctx: &Ctx, r: &mut Report) {
    // SELFTEST FIRST, as its siblings do.
    if ctx.selftest {
        r.skip("objc swizzle drive (selftest: not executed)");
        return;
    }
    if !cfg!(target_os = "macos") {
        r.skip("objc swizzle drive (macOS only: the driven NSApplication is a macOS one)");
        return;
    }
    if !ctx.tools.have_targo() {
        r.skip("objc swizzle drive (no targo)");
        return;
    }
    let build = exec::run(&targo(ctx, objc_swizzle_drive_build_args()), ctx.exec_env());
    if !build.ok {
        r.raw(build.output.as_str());
        r.fail(format!(
            "targo build -p aterm-objc --example {OBJC_SWIZZLE_DRIVE_EXAMPLE}"
        ));
        return;
    }
    // An `aterm-objc` example lands under the same `<target>/debug/examples/`
    // as the `aterm-gui` ones: cargo's layout is per target dir, not per crate.
    let bin = debug_example(
        &ctx.root,
        ctx.env.cargo_target_dir.as_deref(),
        OBJC_SWIZZLE_DRIVE_EXAMPLE,
    );
    if !is_executable_file(&bin) {
        r.cannot_run(format!(
            "objc swizzle drive: just-built driver missing ({})",
            bin.display()
        ));
        return;
    }
    // A BINARY, never `targo run`, as its siblings.
    let out = exec::run(&Cmd::new(&bin), ctx.exec_env());
    r.raw(out.output.as_str());
    let (outcome, label) = objc_swizzle_outcome(out.code);
    r.record(outcome, label);
}

fn objc_bound_drive(ctx: &Ctx, r: &mut Report) {
    // SELFTEST FIRST, as its siblings do.
    if ctx.selftest {
        r.skip("objc bound drive (selftest: not executed)");
        return;
    }
    if !cfg!(target_os = "macos") {
        r.skip("objc bound drive (macOS only: libdispatch's main queue is a Darwin one)");
        return;
    }
    if !ctx.tools.have_targo() {
        r.skip("objc bound drive (no targo)");
        return;
    }
    let build = exec::run(&targo(ctx, objc_bound_drive_build_args()), ctx.exec_env());
    if !build.ok {
        r.raw(build.output.as_str());
        r.fail(format!(
            "targo build -p aterm-objc --example {OBJC_BOUND_DRIVE_EXAMPLE}"
        ));
        return;
    }
    let bin = debug_example(
        &ctx.root,
        ctx.env.cargo_target_dir.as_deref(),
        OBJC_BOUND_DRIVE_EXAMPLE,
    );
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
    r.record(outcome, label);
}

// ---------------------------------------------------------------------------
// 6) --full ONLY: differential oracle
// ---------------------------------------------------------------------------
fn differential_oracle(ctx: &Ctx, r: &mut Report) {
    if ctx.selftest {
        r.skip("differential oracle (selftest)");
        return;
    }
    if !ctx.tools.have_targo() {
        r.skip("differential oracle (no targo)");
        return;
    }
    run_labeled(
        ctx,
        r,
        "targo test -p aterm-bench --test differential",
        &targo(ctx, differential_args()),
    );
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
/// `--full` only: every forge cell type-checked FOR ITS OWN TRIPLE.
///
/// The rest of this gate compiles aterm for one target. aterm ships five, and
/// until 2026-09-01 the other four were held by source reading — which is where
/// both defects the `once_cell` judge found had been living. `xtask gate cells`
/// runs a real compiler per triple, on a toolchain that carries that std, from
/// a cwd and into a target directory OUTSIDE this repo. A cell whose toolchain
/// is not installed SKIPS inside the verb and says out loud that nothing was
/// compiled for it; a cell that runs and fails is a FAILURE, never re-read as a
/// skip.
///
/// AND IT READS THIS REPO'S OWN CODE ON ALL FIVE. For the first day of its life
/// the verb was GREEN on linux and win while neither cell had type-checked one
/// line of aterm's first-party crates: `ring` and `zstd-sys` bundle C, their
/// build scripts could not run for those triples, and the excuse for that took
/// eighteen crates with it. They are SHIMMED now (`tools/cross-cell-gate.tsv`,
/// `cshim` rows), and each cell FAILS if any in-repo package in its graph goes
/// unread — an obligation with no escape hatch in the policy file.
fn cross_cells(ctx: &Ctx, r: &mut Report) {
    if ctx.selftest {
        r.skip("cross-cell type-check (selftest)");
        return;
    }
    if !ctx.tools.have_targo() {
        r.skip("cross-cell type-check (no targo)");
        return;
    }
    run_labeled(ctx, r, "gate cells", &targo(ctx, xtask_gate_args("cells")));
}

fn kani_floor(ctx: &Ctx, r: &mut Report) {
    if ctx.selftest {
        r.skip("trust-mc (selftest)");
        return;
    }
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
        r.raw("          fix: `aterm pkg install trust-mc` (`aterm pkg doctor` names the store);");
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
        let (outcome, why) = kani_floor_outcome(out.ok, &out.output);
        r.record(outcome, format!("verify-kani-proofs.sh ({krate}){why}"));
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
            false,
            EnvSnapshot::default(),
            PathBuf::from("/tmp"),
        )
    }

    #[test]
    fn every_driver_invocation_names_its_lane() {
        // `targo` REFUSES a bare verb on purpose: an artifact is either verified
        // or explicitly unverified, never implicitly one of them. A ported
        // command that dropped `--unverified` would not run at all — but one that
        // grew a bare `cargo` fallback would silently make the gate meaningless.
        let s = Scope::workspace();
        for argv in [
            build_args(&s),
            test_args(&s),
            doctest_args(&s),
            regex_lane_args(),
            xtask_gate_args("drift"),
            freeze_gate_args(),
            differential_args(),
            redraw_conformance_build_args(),
        ] {
            assert_eq!(
                argv.first().map(String::as_str),
                Some("--unverified"),
                "{argv:?}"
            );
        }
    }

    #[test]
    fn libc_oracle_owns_an_absolute_target_dir_and_suppresses_python_bytecode() {
        for caller_target in ["caller-relative", "/caller/absolute-target"] {
            let mut c = ctx(Scope::workspace());
            c.env.cargo_target_dir = Some(caller_target.into());
            let cmd = libc_oracle_cmd(&c).expect("the absolute fixture root resolves");
            assert_eq!(cmd.program, PathBuf::from("/repo/libc-oracle/run.sh"));
            assert_eq!(
                cmd.envs,
                [
                    ("CARGO_TARGET_DIR".into(), "/repo/libc-oracle/target".into()),
                    ("PYTHONDONTWRITEBYTECODE".into(), "1".into()),
                ],
                "the required lane must ignore caller CARGO_TARGET_DIR={caller_target}"
            );
        }

        let mut relative = ctx(Scope::workspace());
        relative.root = PathBuf::from("relative-repo");
        let cmd = libc_oracle_cmd(&relative).expect("a relative repo root can be absolutized");
        let target = &cmd.envs[0].1;
        assert!(
            PathBuf::from(target).is_absolute(),
            "run.sh executes cross commands from /, so this cannot be relative: {target:?}"
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
    fn a_machine_with_no_doc_driver_anywhere_is_diagnosed_not_left_to_die_at_exec() {
        // targo exists, trustdoc does not, and nothing on the children's PATH
        // answers to the bare name: the test stage must say COULD-NOT-RUN with
        // the remedy BEFORE spawning anything (the old behavior ran the child
        // and let cargo die at exec mid-stage, after the unit tests passed),
        // and the doctest stage points at that line rather than re-declaring.
        let mut cc = ctx(Scope::workspace());
        cc.tools.targo = PathBuf::from("/bin/sh");
        cc.tools.trustdoc = PathBuf::from("/nonexistent/trustdoc");
        cc.path_env = "/nonexistent-dir".into();
        let spec = |id| StageSpec {
            id,
            title: "t".into(),
            lane: crate::plan::Lane::MainTarget,
            exclusive: false,
        };

        let r = run_stage(&cc, &spec(StageId::Test));
        let outcomes: Vec<_> = r.outcomes().collect();
        assert_eq!(outcomes.len(), 1, "test decided once");
        assert_eq!(outcomes[0].0, Outcome::Fail(Severity::CouldNotRun));
        assert!(
            outcomes[0].1.contains("~/.local/bin/trustdoc"),
            "the diagnosis names the remedy: {}",
            outcomes[0].1
        );

        let r = run_stage(&cc, &spec(StageId::Doctests));
        let outcomes: Vec<_> = r.outcomes().collect();
        assert_eq!(outcomes.len(), 1, "doctests decided once");
        assert_eq!(outcomes[0].0, Outcome::Skip);
        assert!(
            outcomes[0].1.contains("see the test line"),
            "{}",
            outcomes[0].1
        );

        // The regex lane compiles aterm-search doctests, so it takes the same
        // rule — a skip pointing at the test line, never a raw exec death that
        // would land as GateFailed and outrank the honest COULD-NOT-RUN.
        let r = run_stage(&cc, &spec(StageId::RegexLane));
        let outcomes: Vec<_> = r.outcomes().collect();
        assert_eq!(outcomes.len(), 1, "regex lane decided once");
        assert_eq!(outcomes[0].0, Outcome::Skip);
        assert!(
            outcomes[0].1.contains("see the test line"),
            "{}",
            outcomes[0].1
        );

        // `--selftest` executes nothing, so there is no machine to diagnose:
        // the ladder keeps its uniform selftest skips and the run stays green.
        let mut cs = ctx(Scope::workspace());
        cs.tools.targo = PathBuf::from("/bin/sh");
        cs.tools.trustdoc = PathBuf::from("/nonexistent/trustdoc");
        cs.path_env = "/nonexistent-dir".into();
        cs.selftest = true;
        let r = run_stage(&cs, &spec(StageId::Test));
        let outcomes: Vec<_> = r.outcomes().collect();
        assert_eq!(outcomes.len(), 1, "selftest test decided once");
        assert_eq!(outcomes[0].0, Outcome::Skip);
        assert!(
            outcomes[0].1.contains("(selftest: not executed)"),
            "{}",
            outcomes[0].1
        );

        // A cone with no lib target compiles no doctests — rustdoc is never
        // spawned, so the run proceeds instead of blaming a tool it never
        // needed. `/usr/bin/true` stands in for targo: an Ok outcome proves
        // the stage RAN the child rather than declaring COULD-NOT-RUN.
        let mut cb = ctx(Scope::changed("main", vec!["xtask".into()], false));
        cb.tools.targo = PathBuf::from("/usr/bin/true");
        cb.tools.trustdoc = PathBuf::from("/nonexistent/trustdoc");
        cb.path_env = "/nonexistent-dir".into();
        // The ctx() helper's `/repo` root is fine for stages that never spawn;
        // this case must actually exec, so the child needs a real cwd.
        cb.root = std::env::temp_dir();
        cb.scratch = std::env::temp_dir();
        let r = run_stage(&cb, &spec(StageId::Test));
        let outcomes: Vec<_> = r.outcomes().collect();
        assert_eq!(outcomes.len(), 1, "bin-only test decided once");
        assert_eq!(outcomes[0].0, Outcome::Ok, "{}", outcomes[0].1);

        // `--scope <crate>` answers the lib question from the member's own
        // manifest: a bin-only scope runs (rustdoc never spawns), and the
        // same scope with a lib is a real diagnosis.
        let tmp = crate::mktemp_dir("atv-doc-scope").expect("mktemp");
        let bin_only = tmp.join("crates/binonly");
        std::fs::create_dir_all(bin_only.join("src")).unwrap();
        std::fs::write(
            bin_only.join("Cargo.toml"),
            "[package]\nname = \"binonly\"\n",
        )
        .unwrap();
        let mut cx = ctx(Scope::Crate("binonly".into()));
        cx.tools.targo = PathBuf::from("/usr/bin/true");
        cx.tools.trustdoc = PathBuf::from("/nonexistent/trustdoc");
        cx.path_env = "/nonexistent-dir".into();
        cx.root = tmp.clone();
        cx.scratch = std::env::temp_dir();
        let r = run_stage(&cx, &spec(StageId::Test));
        let outcomes: Vec<_> = r.outcomes().collect();
        assert_eq!(outcomes[0].0, Outcome::Ok, "{}", outcomes[0].1);

        std::fs::write(bin_only.join("src/lib.rs"), "").unwrap();
        let r = run_stage(&cx, &spec(StageId::Test));
        let outcomes: Vec<_> = r.outcomes().collect();
        assert_eq!(
            outcomes[0].0,
            Outcome::Fail(Severity::CouldNotRun),
            "{}",
            outcomes[0].1
        );
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
            test_args(&Scope::workspace()),
            test_args(&Scope::crate_only("aterm-grid")),
            test_args(&Scope::changed("main", vec!["aterm-gui".into()], true)),
            doctest_args(&Scope::workspace()),
            doctest_args(&Scope::crate_only("aterm-grid")),
            regex_lane_args(),
        ] {
            assert!(
                argv.iter().any(|a| a == "--no-fail-fast"),
                "a test argv without --no-fail-fast reports a PREFIX of its \
                 scope: {argv:?}"
            );
        }
    }

    #[test]
    fn the_workspace_argv_is_what_the_script_ran() {
        let s = Scope::workspace();
        assert_eq!(build_args(&s), ["--unverified", "build", "--workspace"]);
        assert_eq!(
            test_args(&s),
            ["--unverified", "test", "--workspace", "--no-fail-fast"]
        );
        assert_eq!(
            doctest_args(&s),
            [
                "--unverified",
                "test",
                "--doc",
                "--workspace",
                "--no-fail-fast"
            ]
        );
        assert_eq!(
            regex_lane_args(),
            [
                "--unverified",
                "test",
                "-p",
                "aterm-search",
                "--features",
                "regex",
                "--no-fail-fast"
            ]
        );
        assert_eq!(
            xtask_gate_args("mainloop"),
            [
                "--unverified",
                "run",
                "-q",
                "-p",
                "xtask",
                "--",
                "gate",
                "mainloop"
            ]
        );
        assert_eq!(
            freeze_gate_args(),
            [
                "--unverified",
                "build",
                "--manifest-path",
                "tools/freeze-safety-gate/Cargo.toml"
            ]
        );
        assert_eq!(
            differential_args(),
            [
                "--unverified",
                "test",
                "-p",
                "aterm-bench",
                "--test",
                "differential"
            ]
        );
        assert_eq!(
            libc_oracle_cmd(&ctx(Scope::workspace()))
                .expect("absolute fixture root")
                .argv(),
            ["/repo/libc-oracle/run.sh"]
        );
        assert_eq!(
            tippy_args(&s),
            [
                "--workspace",
                "--all-targets",
                "--keep-going",
                "--",
                "-D",
                "warnings"
            ]
        );
    }

    #[test]
    fn a_scope_reaches_the_build_test_doctest_and_lint_argv_together() {
        let s = Scope::crate_only("aterm-grid");
        assert_eq!(
            build_args(&s),
            ["--unverified", "build", "-p", "aterm-grid"]
        );
        assert_eq!(
            test_args(&s),
            ["--unverified", "test", "-p", "aterm-grid", "--no-fail-fast"]
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
        // and NOT the whole-tree stages
        assert_eq!(
            regex_lane_args(),
            [
                "--unverified",
                "test",
                "-p",
                "aterm-search",
                "--features",
                "regex",
                "--no-fail-fast"
            ],
            "the regex lane is always that crate, or it is not run at all"
        );
    }

    #[test]
    fn a_change_scope_reaches_the_same_four_argvs_as_one_dash_p_per_crate() {
        let s = Scope::changed("main", vec!["aterm-grid".into(), "aterm-gui".into()], true);
        assert_eq!(
            build_args(&s),
            [
                "--unverified",
                "build",
                "-p",
                "aterm-grid",
                "-p",
                "aterm-gui"
            ]
        );
        assert_eq!(
            test_args(&s),
            [
                "--unverified",
                "test",
                "-p",
                "aterm-grid",
                "-p",
                "aterm-gui",
                "--no-fail-fast"
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
            StageId::Build,
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
    }

    #[test]
    fn tippy_keeps_its_own_target_dir_and_migration_quiet() {
        // Both are load-bearing: a shared target dir would churn the stock build
        // on every gate run, and the migration warning is noise the lint would
        // otherwise turn into a failure under -D warnings.
        let mut c = ctx(Scope::workspace());
        c.tools.tippy = Some(PathBuf::from("/s2/targo-tippy"));
        c.path_env = std::ffi::OsString::from("/usr/bin");
        let mut r = Report::new("t");
        // selftest short-circuits before spawning, so this exercises construction
        // only; the environment is asserted from the same code path below.
        c.selftest = true;
        tippy(&c, &mut r);
        assert_eq!(r.outcomes().count(), 1);

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

    #[test]
    fn the_gated_pass_names_every_required_features_target_and_only_those() {
        let argv = tippy_gated_args(&Scope::workspace()).expect("the workspace selects both");
        assert_eq!(
            argv,
            [
                "-p",
                "aterm-gui",
                "-p",
                "aterm-scrollback",
                "--features",
                "aterm-gui/bench-support,aterm-gui/control-conformance,\
                 aterm-scrollback/disk-tier",
                "--all-targets",
                "--keep-going",
                "--",
                "-D",
                "warnings",
            ],
            "the second pass turns on exactly the features that unlock the six \
             `required-features` targets, and keeps going past a red one"
        );
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
    fn the_regex_lane_carries_the_marker_that_arms_the_regex_tests() {
        // Without `ATERM_SEARCH_REGEX_LANE` the aterm-search suite still runs
        // and still passes — green, with no regex coverage. The marker IS the
        // stage; asserting it on the command the stage builds is the only way
        // to notice it going missing.
        let c = ctx(Scope::workspace());
        let cmd = regex_lane_cmd(&c);
        assert_eq!(env_names(&cmd), ["ATERM_SEARCH_REGEX_LANE"]);
        assert_eq!(cmd.envs[0].1.to_string_lossy(), "1");
        let args: Vec<String> = cmd
            .args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, regex_lane_args());
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
        // One script, three runs, distinguished ONLY by `KANI_CRATE`. Drop it
        // and all three iterations prove the same crate while the ladder still
        // prints three green rows — three claims, one of them true.
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
    fn trust_mc_and_ay_resolve_env_then_store_then_source() {
        // The order the script uses, decided in one place so the availability
        // check and the run can never disagree. Explicit env wins outright; the
        // atpkg store is taken only when it really holds the tool; the
        // from-source dev build is the last resort.
        let home = crate::mktemp_dir("atv-kani").expect("mktemp");
        let prefix = crate::toolchain::default_atpkg_prefix(&home);
        let env = EnvSnapshot {
            home: home.clone(),
            ..EnvSnapshot::default()
        };
        assert_eq!(
            trust_mc_sysroot(&env),
            home.join("trust/first-party/trust-mc/target/trust-mc"),
            "no store, no override: the dev default"
        );
        assert_eq!(
            ay_bin_dir(&env),
            home.join("trust/first-party/ay/target/release")
        );

        // `aterm pkg install trust-mc` / `ay` shape: the live-build link for the
        // sysroot, the shim dir for the solver.
        let mc = prefix.join("store/trust-mc/current/bin");
        std::fs::create_dir_all(&mc).expect("mkdir");
        std::fs::write(mc.join("trust-mc-driver"), b"#!/bin/sh\nexit 0\n").expect("write");
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                mc.join("trust-mc-driver"),
                std::fs::Permissions::from_mode(0o755),
            )
            .expect("chmod");
        }
        let shims = prefix.join("bin");
        std::fs::create_dir_all(&shims).expect("mkdir");
        std::fs::write(shims.join("ay"), b"#!/bin/sh\nexit 0\n").expect("write");
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(shims.join("ay"), std::fs::Permissions::from_mode(0o755))
                .expect("chmod");
        }
        assert_eq!(
            trust_mc_sysroot(&env),
            prefix.join("store/trust-mc/current")
        );
        assert_eq!(ay_bin_dir(&env), shims);

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
    fn no_targo_is_fail_closed_at_the_build_and_honest_everywhere_after() {
        let c = ctx(Scope::workspace());
        assert!(!c.tools.have_targo());

        let mut r = Report::new("build");
        build(&c, &mut r);
        let (outcome, label) = r.outcomes().next().expect("a decision");
        assert_eq!(outcome, crate::Outcome::Fail(crate::Severity::CouldNotRun));
        assert!(label.starts_with("targo not found at "), "{label}");
        assert!(
            label.contains("x.py build --stage 2"),
            "the diagnostic says how to fix it"
        );

        // The dependent stages then skip — honestly, and named, so the verdict
        // refuses the merge contract for the whole run.
        let dependent: [fn(&Ctx, &mut Report); 7] = [
            test,
            doctests,
            regex_lane,
            feature_gates,
            freeze_gate,
            proof_inventory,
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

    #[test]
    fn a_missing_helper_script_can_never_pass() {
        let c = ctx(Scope::workspace());
        let script_stages: [fn(&Ctx, &mut Report); 4] =
            [grep_guards, install_channel, start_compare, license_headers];
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

    #[test]
    fn selftest_executes_nothing_heavy_and_says_so() {
        let mut c = ctx(Scope::workspace());
        c.selftest = true;
        c.tools.targo = PathBuf::from("/bin/sh"); // pretend a driver exists
        let mut r = Report::new("build");
        build(&c, &mut r);
        assert_eq!(
            r.outcomes().collect::<Vec<_>>(),
            [(
                crate::Outcome::Skip,
                "targo build --workspace (selftest: not executed)"
            )]
        );
    }
}
